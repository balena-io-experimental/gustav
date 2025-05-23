use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::{anyhow, Context as AnyhowCtx};
use json_patch::Patch;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::{debug, debug_span, error, field, instrument, warn, Level, Span};

use crate::errors::{InternalError, MethodError, SerializationError};
use crate::path::Path;
use crate::system::System;
use crate::task::{self, Context, Operation, Task};
use crate::workflow::{WorkUnit, Workflow};
use crate::Dag;

mod distance;
mod domain;

use distance::*;
pub use domain::*;

#[derive(Debug)]
pub struct Planner(Domain);

#[derive(Debug, Error)]
enum SearchFailed {
    #[error("method error: {0}")]
    BadMethod(#[from] PathSearchError),

    #[error("task error: {0:?}")]
    BadTask(#[from] task::Error),

    #[error("task empty")]
    EmptyTask,

    #[error("loop detected")]
    LoopDetected,

    // this is probably a bug if this error
    // happens
    #[error("internal error: {0:?}")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Serialization(#[from] SerializationError),

    #[error(transparent)]
    Task(#[from] task::Error),

    #[error("workflow not found")]
    NotFound,

    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// Returns the longest (maximum cardinality) subset of non‐conflicting paths
/// from the input. Two paths are considered to conflict if one is a prefix
/// of the other. In such a case, we choose to keep the path that is not a prefix
/// of any other—that is, the more specific path.
///
/// # Arguments
///
/// * `paths` - A vector of `Path` instances.
///
/// # Returns
///
/// A vector of `Path` which represents one maximal set (greedy solution)
/// in which no path is a prefix of another.
fn longest_non_conflicting(paths: Vec<Path>) -> Vec<Path> {
    // We use a simple O(n^2) approach:
    // For each path, if any other path starts with it, then skip it.
    // TODO: maybe implement a more efficient algorithm with a trie
    let mut result = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        // Check p against every other path in the input.
        let mut p_is_prefix = false;
        for (j, q) in paths.iter().enumerate() {
            if i == j {
                continue;
            }
            // If p is a proper prefix of q, then skip p.
            if q.to_str().starts_with(p.to_str()) {
                p_is_prefix = true;
                break;
            }
        }
        if !p_is_prefix {
            result.push(p.clone());
        }
    }
    result
}

impl Planner {
    pub fn new(domain: Domain) -> Self {
        Self(domain)
    }

    pub fn domain(&self) -> &Domain {
        &self.0
    }

    #[instrument(level = "trace", skip_all, fields(task=?task, changes=field::Empty), err(level=Level::TRACE))]
    fn try_task(
        &self,
        task: Task,
        cur_state: &System,
        cur_plan: Workflow,
        stack_len: u32,
    ) -> Result<Workflow, SearchFailed> {
        match task {
            Task::Action(action) => {
                let work_id = WorkUnit::new_id(&action, cur_state.root());

                // Detect loops first, if the same action is being applied to the same
                // state then abort this search branch
                if cur_plan.as_dag().some(|a| a.id == work_id) {
                    return Err(SearchFailed::LoopDetected)?;
                }

                // Simulate the task and get the list of changes
                let Patch(changes) = action.dry_run(cur_state).map_err(SearchFailed::BadTask)?;
                if changes.is_empty() {
                    return Err(SearchFailed::EmptyTask);
                }

                let Workflow { dag, pending } = cur_plan;

                // Append a new node to the workflow, include a copy
                // of the changes for validation during runtime
                let dag = dag.concat(Dag::from_sequence([WorkUnit::new(
                    work_id,
                    action,
                    changes.clone(),
                )]));

                Span::current().record("changes", format!("{:?}", changes));
                let pending = [pending, changes].concat();

                Span::current().record("selected", true);
                Ok(Workflow { dag, pending })
            }
            Task::Method(method) => {
                // Get the list of referenced tasks
                let tasks = method.expand(cur_state).map_err(SearchFailed::BadTask)?;

                // Extended tasks will store the correct references from the domain with the
                // right path and description
                let mut extended_tasks = Vec::new();

                for mut t in tasks.into_iter() {
                    // The subtask is not allowed to override arguments
                    // in the parent task, so we first make sure to propagate
                    // arguments from the parent
                    for (k, v) in method.context().args.iter() {
                        t = t.with_arg(k, v);
                    }

                    let task_id = t.id().to_string();
                    let Context { args, .. } = t.context_mut();

                    // Find the job path on the domain list
                    let path = self.0.find_path_for_job(&task_id, args)?;

                    // Using the path, now find the actual job on the domain.
                    // The domain job includes metadata like the description that
                    // we want to use in the workflow
                    let job = self
                        .0
                        .find_job(&path, &task_id)
                        // this should never happen
                        .ok_or(anyhow!("failed to find job for path {path}"))?;

                    // Get a copy of the task for the final list
                    let task = job
                        .clone_task(t.context().to_owned())
                        .with_path(path.clone());

                    extended_tasks.push(task);
                }

                // Compute a maximal set of non-overlapping (non-prefix) paths for parallelism
                let non_conflicting_paths = longest_non_conflicting(
                    extended_tasks.iter().map(|t| t.path().clone()).collect(),
                );

                // The method is parallelizable if all task paths are in the non-conflicting list
                // and are all scoped (i.e. none of them requires access to System)
                let mut parallelizable = true;
                for task in &extended_tasks {
                    if !task.is_scoped() || !non_conflicting_paths.contains(task.path()) {
                        parallelizable = false;
                        break;
                    }
                }

                let mut changes = vec![];
                let mut cur_state = cur_state.clone();
                let mut cur_plan = cur_plan;

                if parallelizable {
                    let mut branches = vec![];

                    // Create a branch for each task
                    for task in extended_tasks {
                        let Workflow { dag, pending } =
                            self.try_task(task, &cur_state, Workflow::default(), stack_len + 1)?;

                        changes.extend(pending);
                        branches.push(dag);
                    }

                    // Extend the current plan with the forking dag
                    cur_plan = Workflow {
                        dag: cur_plan.dag.concat(Dag::from_branches(branches)),
                        pending: vec![],
                    }
                } else {
                    // If the task is not parallelizable, run tasks in sequence making
                    // sure to apply changes before calling the next task
                    for task in extended_tasks {
                        let Workflow { dag, pending } =
                            self.try_task(task, &cur_state, cur_plan, stack_len + 1)?;

                        cur_state
                            .patch(Patch(pending.clone()))
                            .with_context(|| format!("failed to apply patch {pending:?}"))?;

                        changes.extend(pending);
                        cur_plan = Workflow {
                            dag,
                            pending: vec![],
                        };
                    }
                }

                // Include changes in the returned plan
                cur_plan.pending = changes;
                Ok(cur_plan)
            }
        }
    }

    #[instrument(skip_all, fields(ini=%system.root(), tgt=%tgt), err, ret(Display))]
    pub(crate) fn find_workflow<T>(&self, system: &System, tgt: &Value) -> Result<Workflow, Error>
    where
        T: Serialize + DeserializeOwned,
    {
        // The search stack stores (current_state, current_plan, depth)
        let mut stack = vec![(system.clone(), Workflow::default(), 0)];
        let find_workflow_span = Span::current();

        while let Some((cur_state, cur_plan, depth)) = stack.pop() {
            // Prevent infinite recursion (e.g., from buggy tasks or recursive methods)
            if depth >= 256 {
                warn!(parent: &find_workflow_span, "reached max search depth (256)");
                return Err(Error::NotFound)?;
            }

            // Normalize state: deserialize into T and re-serialize to remove internal fields
            let cur = cur_state
                .state::<T>()
                .and_then(System::try_from)
                .map_err(SerializationError::from)?;

            // Compute the difference between current and target state
            let distance = Distance::new(&cur, tgt);

            // If no difference, we’ve reached the goal
            if distance.is_empty() {
                return Ok(cur_plan);
            }

            // Compute a maximal set of non-overlapping (non-prefix) paths for parallelism
            let non_conflicting_paths =
                longest_non_conflicting(distance.iter().map(|op| Path::new(op.path())).collect());

            // For each path, store the best-scoped task workflow that can be used in a parallel branch
            let mut non_conflicting = BTreeMap::new();
            for path in non_conflicting_paths {
                non_conflicting.insert(path, Vec::<Workflow>::new());
            }

            // Prioritize forking paths coming from methods instead of
            // parallelism from non conflicting paths
            // TODO: we probably need a way to prioritize candidates, perhaps
            // by number of changes rather than by degree or job priority
            let mut has_parallel = false;

            let next_span = debug_span!("find_next", cur = %&cur_state.root());
            let _enter = next_span.enter();
            let mut candidates = Vec::new();

            // Iterate over distance operations in reverse (deeper paths first for proper task ordering)
            for op in distance.iter().rev() {
                let path = Path::new(op.path());

                // Retrieve matching jobs at this path
                if let Some((args, jobs)) = self.0.find_matching_jobs(path.to_str()) {
                    let pointer = path.as_ref();
                    let target = pointer.resolve(tgt).unwrap_or(&Value::Null);

                    let context = Context {
                        path: path.clone(),
                        args,
                        target: target.clone(),
                    };

                    // Reverse job list to put higher priority jobs at the top of the
                    // stack first
                    for job in jobs.filter(|j| j.operation() != &Operation::None).rev() {
                        if op.matches(job.operation()) || job.operation() == &Operation::Any {
                            let task = job.clone_task(context.clone());
                            let task_id = task.id().to_string();
                            let task_is_scoped = task.is_scoped();
                            let task_description = task.to_string();

                            // Try applying this task to the current state
                            match self.try_task(task.clone(), &cur_state, Workflow::default(), 0) {
                                Ok(Workflow { dag, pending }) if !pending.is_empty() => {
                                    // Apply resulting changes to a cloned system state
                                    let mut new_sys = cur_state.clone();
                                    new_sys
                                        .patch(Patch(pending.clone()))
                                        .with_context(|| "failed to apply patch")
                                        .map_err(InternalError::from)?;

                                    if dag.is_forking() {
                                        has_parallel = dag.is_forking();
                                    }

                                    // Extend current plan with new task
                                    let new_plan = Workflow {
                                        dag: cur_plan.dag.clone().concat(dag.clone()),
                                        pending: vec![],
                                    };

                                    // Record scoped task workflows for parallel fork candidates
                                    if let Some(workflows) = non_conflicting.get_mut(&path) {
                                        if task_is_scoped {
                                            workflows.push(Workflow { dag, pending });
                                        }
                                    }

                                    // Add updated plan/state to the search stack

                                    candidates.push(task_description);
                                    stack.push((new_sys, new_plan, depth + 1));
                                }

                                // Non-critical errors are ignored (loop, empty, condition failure)
                                Err(SearchFailed::LoopDetected)
                                | Err(SearchFailed::EmptyTask)
                                | Err(SearchFailed::BadTask(task::Error::ConditionFailed)) => {}

                                // Critical internal errors terminate the search
                                Err(SearchFailed::Internal(err)) => {
                                    return Err(InternalError::from(err))?;
                                }

                                // Method expansion failure
                                Err(SearchFailed::BadMethod(err)) => {
                                    let err = MethodError::new(err);
                                    if cfg!(debug_assertions) {
                                        return Err(task::Error::from(err))?;
                                    }
                                    warn!(parent: &find_workflow_span, "task {} failed: {} ... ignoring", task_id, err);
                                }

                                // Other task failure (non-debug: warn and skip)
                                Err(SearchFailed::BadTask(err)) => {
                                    if cfg!(debug_assertions) {
                                        return Err(err)?;
                                    }
                                    warn!(parent: &find_workflow_span, "task {} failed: {} ... ignoring", task_id, err);
                                }

                                _ => {}
                            }
                        }
                    }
                }
            }

            // Log task candidates (with branch ordering) if debugging
            if tracing::event_enabled!(Level::DEBUG) {
                let joined = candidates
                    .iter()
                    .map(ToString::to_string)
                    .rev()
                    .collect::<Vec<_>>()
                    .join(", ");
                debug!(parent: &next_span, candidates = %joined);
            }

            // Combine all parallel-compatible workflows into a forked DAG
            let parallel: Vec<Workflow> = non_conflicting
                .values()
                .filter_map(|v| v.first().cloned())
                .collect();

            // Only add parallel option if there is no parallel method in
            // the generated list
            if !has_parallel && parallel.len() > 1 {
                let mut changes = Vec::new();
                let mut branches = Vec::new();

                for Workflow { dag, pending } in parallel {
                    changes.extend(pending);
                    branches.push(dag);
                }

                let mut new_sys = cur_state;
                new_sys
                    .patch(Patch(changes))
                    .with_context(|| "failed to apply parallel patch")
                    .map_err(InternalError::from)?;

                // Construct a new DAG with parallel branches joined via `Dag::from_branches`
                let dag = Dag::from_branches(branches);
                let new_plan = Workflow {
                    dag: cur_plan.dag.concat(dag),
                    pending: vec![],
                };

                // Push the parallel workflow candidate to the stack
                stack.push((new_sys, new_plan, depth + 1));
            }
        }

        // No candidate plan reached the goal state
        Err(Error::NotFound)?
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::fmt::Display;

    use super::*;
    use crate::extract::{Args, System, Target, View};
    use crate::{dag, par, task::*};
    use crate::{seq, Dag};
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::{prelude::*, EnvFilter};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Counters(HashMap<String, i32>);

    fn init() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_target(false)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
            )
            .with(EnvFilter::from_default_env())
            .try_init()
            .unwrap_or(());
    }

    fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> View<i32> {
        if *counter < tgt {
            *counter += 1;
        }

        counter
    }

    fn buggy_plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> View<i32> {
        if *counter < tgt {
            // This is the wrong operation
            *counter -= 1;
        }

        counter
    }

    fn plus_two(counter: View<i32>, Target(tgt): Target<i32>) -> Vec<Task> {
        if tgt - *counter > 1 {
            return vec![plus_one.with_target(tgt), plus_one.with_target(tgt)];
        }

        vec![]
    }

    fn plus_three(counter: View<i32>, Target(tgt): Target<i32>) -> Vec<Task> {
        if tgt - *counter > 2 {
            return vec![plus_two.with_target(tgt), plus_one.with_target(tgt)];
        }

        vec![]
    }

    fn minus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> View<i32> {
        if *counter > tgt {
            *counter -= 1;
        }

        counter
    }

    pub fn find_plan<S>(planner: Planner, cur: S, tgt: S) -> Result<Workflow, super::Error>
    where
        S: Serialize + DeserializeOwned,
    {
        let tgt = serde_json::to_value(tgt).expect("failed to serialize target state");

        let system =
            crate::system::System::try_from(cur).expect("failed to serialize current state");

        let res = planner.find_workflow::<S>(&system, &tgt)?;
        Ok(res)
    }

    #[test]
    fn it_calculates_a_linear_workflow() {
        let domain = Domain::new()
            .job("", update(plus_one))
            .job("", update(minus_one));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, 0, 2).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "mahler::planner::tests::plus_one()",
            "mahler::planner::tests::plus_one()"
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_ignores_none_jobs() {
        let domain = Domain::new().job("", none(plus_one));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, 0, 2);

        assert!(matches!(workflow, Err(super::Error::NotFound)));
    }

    #[test]
    fn it_aborts_search_if_plan_length_grows_too_much() {
        let domain = Domain::new()
            .job("", update(buggy_plus_one))
            .job("", update(minus_one));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, 0, 2);
        assert!(workflow.is_err());
    }

    #[test]
    fn it_calculates_a_linear_workflow_with_compound_tasks() {
        let domain = Domain::new()
            .job("", update(plus_two))
            .job("", none(plus_one));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, 0, 2).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "mahler::planner::tests::plus_one()",
            "mahler::planner::tests::plus_one()"
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state() {
        #[derive(Serialize, Deserialize)]
        struct MyState {
            counters: HashMap<String, i32>,
        }

        let initial = MyState {
            counters: HashMap::from([("one".to_string(), 0), ("two".to_string(), 0)]),
        };

        let target = MyState {
            counters: HashMap::from([("one".to_string(), 2), ("two".to_string(), 2)]),
        };

        let domain = Domain::new()
            .job("/counters/{counter}", update(minus_one))
            .job("/counters/{counter}", update(plus_one));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, initial, target).unwrap();

        // We expect counters to be updated in parallel
        let expected: Dag<&str> = par!(
            "mahler::planner::tests::plus_one(/counters/one)",
            "mahler::planner::tests::plus_one(/counters/two)",
        ) + par!(
            "mahler::planner::tests::plus_one(/counters/one)",
            "mahler::planner::tests::plus_one(/counters/two)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state_with_compound_tasks() {
        #[derive(Serialize, Deserialize)]
        struct MyState {
            counters: HashMap<String, i32>,
        }

        let initial = MyState {
            counters: HashMap::from([("one".to_string(), 0), ("two".to_string(), 0)]),
        };

        let target = MyState {
            counters: HashMap::from([("one".to_string(), 2), ("two".to_string(), 2)]),
        };

        let domain = Domain::new()
            .job("/counters/{counter}", none(plus_one))
            .job("/counters/{counter}", update(plus_two));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, initial, target).unwrap();

        // We expect a parallel dag with two tasks on each branch
        let expected: Dag<&str> = dag!(
            seq!(
                "mahler::planner::tests::plus_one(/counters/one)",
                "mahler::planner::tests::plus_one(/counters/one)",
            ),
            seq!(
                "mahler::planner::tests::plus_one(/counters/two)",
                "mahler::planner::tests::plus_one(/counters/two)",
            )
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state_with_deep_compound_tasks() {
        #[derive(Serialize, Deserialize)]
        struct MyState {
            counters: HashMap<String, i32>,
        }

        let initial = MyState {
            counters: HashMap::from([("one".to_string(), 0), ("two".to_string(), 0)]),
        };

        let target = MyState {
            counters: HashMap::from([("one".to_string(), 3), ("two".to_string(), 0)]),
        };

        let domain = Domain::new()
            .job("/counters/{counter}", none(plus_one))
            .job("/counters/{counter}", none(plus_two))
            .job("/counters/{counter}", update(plus_three));

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, initial, target).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "mahler::planner::tests::plus_one(/counters/one)",
            "mahler::planner::tests::plus_one(/counters/one)",
            "mahler::planner::tests::plus_one(/counters/one)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_finds_parallel_plans_with_nested_forks() {
        init();
        type Counters = BTreeMap<String, i32>;

        #[derive(Serialize, Deserialize, Debug)]
        struct MyState {
            counters: Counters,
        }

        // This is a very dumb example to test how the planner
        // choses parallelizable methods over automated parallelization
        fn multi_increment(counters: View<Counters>, target: Target<Counters>) -> Vec<Task> {
            counters
                .keys()
                .filter(|k| {
                    target.get(k.as_str()).unwrap_or(&0) - counters.get(k.as_str()).unwrap_or(&0)
                        > 1
                })
                .map(|k| {
                    plus_two
                        .with_arg("counter", k)
                        .with_target(target.get(k.as_str()))
                })
                .collect::<Vec<Task>>()
        }

        fn chunker(counters: View<Counters>, target: Target<Counters>) -> Vec<Task> {
            let to_update = counters
                .keys()
                .filter(|k| {
                    target.get(k.as_str()).unwrap_or(&0) - counters.get(k.as_str()).unwrap_or(&0)
                        > 1
                })
                .collect::<Vec<&String>>();

            let mut tasks: Vec<Task> = Vec::new();
            for chunk in to_update.chunks(2) {
                let mut tgt = (*counters).clone();
                for k in chunk {
                    if target.contains_key(k.as_str()) {
                        tgt.insert(k.to_string(), *target.get(k.as_str()).unwrap_or(&0));
                    }
                }
                tasks.push(multi_increment.with_target(tgt));
            }

            tasks
        }

        let domain = Domain::new()
            .job(
                "/counters/{counter}",
                update(plus_one)
                    .with_description(|Args(counter): Args<String>| format!("{counter}++")),
            )
            .job("/counters/{counter}", update(plus_two))
            .job("/counters", update(chunker))
            .job("/counters", update(multi_increment));

        let initial = MyState {
            counters: BTreeMap::from([
                ("a".to_string(), 0),
                ("b".to_string(), 0),
                ("c".to_string(), 0),
                ("d".to_string(), 0),
            ]),
        };

        let target = MyState {
            counters: BTreeMap::from([
                ("a".to_string(), 3),
                ("b".to_string(), 2),
                ("c".to_string(), 2),
                ("d".to_string(), 2),
            ]),
        };

        let planner = Planner::new(domain);
        let workflow = find_plan(planner, initial, target).unwrap();

        // We expect a parallel dag with two tasks on each branch
        let expected: Dag<&str> = dag!(seq!("a++", "a++"), seq!("b++", "b++"))
            + dag!(seq!("c++", "c++"), seq!("d++", "d++"))
            + seq!("a++");

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn test_stacking_problem() {
        init();

        #[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Clone, Debug)]
        enum Block {
            A,
            B,
            C,
        }

        impl Display for Block {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{:?}", self)
            }
        }

        #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
        enum Location {
            Blk(Block),
            Table,
            Hand,
        }

        impl Location {
            fn is_block(&self) -> bool {
                matches!(self, Location::Blk(_))
            }
        }

        type Blocks = HashMap<Block, Location>;

        #[derive(Serialize, Deserialize, Debug)]
        struct State {
            blocks: Blocks,
        }

        fn is_clear(blocks: &Blocks, loc: &Location) -> bool {
            if loc.is_block() || loc == &Location::Hand {
                // No block is on top of the location
                return blocks.iter().all(|(_, l)| l != loc);
            }
            // the table is always clear
            true
        }

        fn is_holding(blocks: &Blocks) -> bool {
            !is_clear(blocks, &Location::Hand)
        }

        fn all_clear(blocks: &Blocks) -> Vec<&Block> {
            blocks
                .iter()
                .filter(|(b, _)| is_clear(blocks, &Location::Blk((*b).clone())))
                .map(|(b, _)| b)
                .collect()
        }

        // Get a block from the table
        fn pickup(
            mut loc: View<Location>,
            System(sys): System<State>,
            Args(block): Args<Block>,
        ) -> View<Location> {
            // if the block is clear and we are not holding any other blocks
            // we can grab the block
            if *loc == Location::Table
                && is_clear(&sys.blocks, &Location::Blk(block))
                && !is_holding(&sys.blocks)
            {
                *loc = Location::Hand;
            }

            loc
        }

        // Unstack a block from other block
        fn unstack(
            mut loc: View<Location>,
            System(sys): System<State>,
            Args(block): Args<Block>,
        ) -> Option<View<Location>> {
            // if the block is clear and we are not holding any other blocks
            // we can grab the block
            if loc.is_block()
                && is_clear(&sys.blocks, &Location::Blk(block))
                && !is_holding(&sys.blocks)
            {
                *loc = Location::Hand;
                return Some(loc);
            }

            None
        }

        // There is really not that much of a difference between putdown and stack
        // this is just to test that the planner can work with nested methods
        fn putdown(mut loc: View<Location>) -> View<Location> {
            // If we are holding the block and the target is clear
            // then we can modify the block location
            if *loc == Location::Hand {
                *loc = Location::Table
            }

            loc
        }

        fn stack(
            mut loc: View<Location>,
            Target(tgt): Target<Location>,
            System(sys): System<State>,
        ) -> View<Location> {
            // If we are holding the block and the target is clear
            // then we can modify the block location
            if *loc == Location::Hand && is_clear(&sys.blocks, &tgt) {
                *loc = tgt
            }

            loc
        }

        fn take(
            loc: View<Location>,
            System(sys): System<State>,
            Args(block): Args<Block>,
        ) -> Option<Task> {
            if is_clear(&sys.blocks, &Location::Blk(block)) {
                if *loc == Location::Table {
                    return Some(pickup.into_task());
                } else {
                    return Some(unstack.into_task());
                }
            }
            None
        }

        fn put(loc: View<Location>, Target(tgt): Target<Location>) -> Option<Task> {
            if *loc == Location::Hand {
                if tgt == Location::Table {
                    return Some(putdown.into_task());
                } else {
                    return Some(stack.with_target(tgt));
                }
            }
            None
        }

        //
        //  This method implements the following block-stacking algorithm [1]:
        //
        //  - If there's a clear block x that can be moved to a place where it won't
        //    need to be moved again, then return a todo list that includes goals to
        //    move it there, followed by mgoal (to achieve the remaining goals).
        //    Otherwise, if there's a clear block x that needs to be moved out of the
        //    way to make another block movable, then return a todo list that includes
        //    goals to move x to the table, followed by mgoal.
        //  - Otherwise, no blocks need to be moved.
        //    [1] N. Gupta and D. S. Nau. On the complexity of blocks-world
        //    planning. Artificial Intelligence 56(2-3):223–254, 1992.
        //
        //  Source: https://github.com/dananau/GTPyhop/blob/main/Examples/blocks_hgn/methods.py
        //
        fn move_blks(blocks: View<Blocks>, Target(target): Target<Blocks>) -> Vec<Task> {
            for blk in all_clear(&blocks) {
                // we assume that the target is well formed
                let tgt_loc = target.get(blk).unwrap();
                let cur_loc = blocks.get(blk).unwrap();

                // The block is free and it can be moved to the final location (another block or the table)
                if cur_loc != tgt_loc && is_clear(&blocks, tgt_loc) {
                    return vec![
                        take.with_arg("block", blk.to_string()),
                        put.with_arg("block", blk.to_string()).with_target(tgt_loc),
                    ];
                }
            }

            // If we get here, no blocks can be moved to the final location so
            // we move them to the table
            let mut to_table: Vec<Task> = vec![];
            for b in all_clear(&blocks) {
                to_table.push(take.with_arg("block", b.to_string()));
                to_table.push(
                    put.with_target(Location::Table)
                        .with_arg("block", b.to_string()),
                );
            }

            to_table
        }
        let domain = Domain::new()
            .jobs(
                "/blocks/{block}",
                [
                    update(pickup).with_description(|Args(block): Args<String>| {
                        format!("pick up block {block}")
                    }),
                    update(unstack).with_description(|Args(block): Args<String>| {
                        format!("unstack block {block}")
                    }),
                    update(putdown).with_description(|Args(block): Args<String>| {
                        format!("put down block {block}")
                    }),
                    update(stack).with_description(
                        |Args(block): Args<String>, Target(tgt): Target<Location>| {
                            let tgt_block = match tgt {
                                Location::Blk(block) => format!("{block:?}"),
                                _ => format!("{tgt:?}"),
                            };

                            format!("stack block {block} on top of block {tgt_block}")
                        },
                    ),
                    update(take),
                    update(put),
                ],
            )
            .job("/blocks", update(move_blks));

        let planner = Planner::new(domain);

        let initial = State {
            blocks: HashMap::from([
                (Block::A, Location::Table),
                (Block::B, Location::Blk(Block::A)),
                (Block::C, Location::Blk(Block::B)),
            ]),
        };
        let target = State {
            blocks: HashMap::from([
                (Block::A, Location::Blk(Block::B)),
                (Block::B, Location::Blk(Block::C)),
                (Block::C, Location::Table),
            ]),
        };

        let workflow = find_plan(planner, initial, target).unwrap();
        let expected: Dag<&str> = seq!(
            "unstack block C",
            "put down block C",
            "unstack block B",
            "stack block B on top of block C",
            "pick up block A",
            "stack block A on top of block B",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }
}

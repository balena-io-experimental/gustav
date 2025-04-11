use anyhow::Context as AnyhowCtx;
use json_patch::Patch;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::{error, field, instrument, trace_span, warn, Level, Span};

use crate::path::Path;
use crate::system::System;
use crate::task::{self, Context, Operation, Task};
use crate::workflow::{WorkUnit, Workflow};

mod distance;
mod domain;

use distance::*;
pub use domain::*;

#[derive(Debug)]
pub struct Planner(Domain);

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("failed to expand compound task: {0}")]
    ExpansionFailed(#[source] PathSearchError),

    #[error("task {id} failed during planning: {source}")]
    TaskFailure { id: String, source: task::Error },

    #[error("condition failed")]
    ConditionFailed,

    #[error("loop detected")]
    LoopDetected,

    #[error("plan not found")]
    NotFound,

    #[error("max search depth reached")]
    MaxDepthReached,

    #[error("unexpected error: {0}")]
    Unexpected(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct NotFound(#[from] anyhow::Error);

impl Planner {
    pub fn new(domain: Domain) -> Self {
        Self(domain)
    }

    #[instrument(level = "trace", skip_all, fields(task=?task, selected=field::Empty), err(level=Level::TRACE))]
    fn try_task(
        &self,
        task: Task,
        cur_state: &System,
        cur_plan: Workflow,
        stack_len: u32,
    ) -> Result<Workflow, Error> {
        let task_id = task.id().to_string();
        match task {
            Task::Action(action) => {
                let work_id = WorkUnit::new_id(&action, cur_state.root()).with_context(|| {
                    format!(
                        "failed to resolve path {} for task {}",
                        action.context().path,
                        task_id
                    )
                })?;

                // Detect loops in the plan
                if cur_plan.as_dag().some(|a| a.id == work_id) {
                    return Err(Error::LoopDetected)?;
                }

                // Test the task
                let Patch(changes) = action.dry_run(cur_state).map_err(|e| Error::TaskFailure {
                    id: task_id,
                    source: e,
                })?;

                // if we are at the top of the stack and no changes are introduced
                // then assume the condition has failed
                if stack_len == 0 && changes.is_empty() {
                    return Err(Error::ConditionFailed)?;
                }

                // Otherwise add the task to the plan
                let Workflow { dag, pending } = cur_plan;
                let dag = dag.with_head(WorkUnit::new(work_id, action, changes.clone()));
                let pending = [pending, changes].concat();

                Span::current().record("selected", true);
                Ok(Workflow { dag, pending })
            }
            Task::Method(method) => {
                let tasks = method.expand(cur_state).map_err(|e| Error::TaskFailure {
                    id: task_id,
                    source: e,
                })?;

                let mut cur_state = cur_state.clone();
                let mut cur_plan = cur_plan;
                for mut t in tasks.into_iter() {
                    // A task cannot override the context set by the
                    // parent task. For instance a method for `/a/b` cannot
                    // have a sub-task for `/a/c`, it can however have a task
                    // for `/a/b` or `/a/b/c`. This is to ensure we can parallelize
                    // tasks
                    for (k, v) in method.context().args.iter() {
                        t = t.with_arg(k, v)
                    }
                    let path = self
                        .0
                        .find_path_for_job(t.id(), &t.context().args)
                        // The user may have not have put the child task in the
                        // domain, or failed to account for all args in the path
                        // in which case we need to return an error
                        .map_err(Error::ExpansionFailed)?;

                    let task = t.with_path(path);
                    let Workflow { dag, pending } =
                        self.try_task(task, &cur_state, cur_plan, stack_len + 1)?;

                    // patch the state before passing it to the next task
                    cur_state
                        .patch(Patch(pending.clone()))
                        .context("failed to patch system state")?;
                    cur_plan = Workflow { dag, pending };
                }

                // if we are at the top of the stack and no changes are introduced
                // then assume the condition has failed
                if stack_len == 0 && cur_plan.pending.is_empty() {
                    return Err(Error::ConditionFailed)?;
                }
                Span::current().record("selected", true);
                Ok(cur_plan)
            }
        }
    }

    pub fn find_plan<S>(&self, cur: S, tgt: S) -> Result<Workflow, NotFound>
    where
        S: Serialize,
    {
        let cur = serde_json::to_value(cur).context("failed to serialize current state")?;
        let tgt = serde_json::to_value(tgt).context("failed to serialize target state")?;

        let system = System::new(cur);
        let res = self
            .find_workflow(&system, &tgt)
            .context("Failed to find a plan")?;
        Ok(res)
    }

    #[instrument(skip_all, fields(ini=%system.root(), tgt=%tgt), err, ret(Display))]
    pub(crate) fn find_workflow(&self, system: &System, tgt: &Value) -> Result<Workflow, Error> {
        // Store the initial state and an empty plan on the stack
        let mut stack = vec![(system.clone(), Workflow::default(), 0)];
        let parent_span = Span::current();

        // TODO: we should merge non conflicting workflows
        // for parallelism
        while let Some((cur_state, cur_plan, depth)) = stack.pop() {
            // we need to limit the search depth to avoid following
            // a buggy task forever
            // TODO: make this configurable
            if depth >= 256 {
                return Err(Error::MaxDepthReached)?;
            }

            let distance = Distance::new(&cur_state, tgt);

            // we reached the target
            if distance.is_empty() {
                // return the existing plan reversing it first
                return Ok(cur_plan.reverse());
            }

            let stack_len = stack.len();
            let next_span =
                trace_span!("find_next", cur = %&cur_state.root(), found = field::Empty);
            let _enter = next_span.enter();
            for op in distance.iter() {
                // Find applicable tasks
                let path = Path::new(op.path());
                let matching = self.0.find_jobs_at(path.to_str());
                if let Some((args, jobs)) = matching {
                    // Calculate the target for the job path
                    let pointer = path.as_ref();

                    // This should technically never happen
                    let target = pointer.resolve(tgt).with_context(|| {
                        format!("failed to resolve path {path} on system state")
                    })?;

                    // Create the calling context for the job
                    let context = Context {
                        path,
                        args,
                        target: target.clone(),
                    };
                    for job in jobs {
                        // If the job is applicable to the operation
                        if op.matches(job.operation()) || job.operation() != &Operation::Any {
                            let task = job.clone_task(context.clone());

                            // apply the task to the state, if it progresses the plan, then select
                            // it and put the new state with the new plan on the stack
                            match self.try_task(task, &cur_state, cur_plan.clone(), 0) {
                                Ok(Workflow { dag, pending }) => {
                                    // If the task is not progressing the plan, we can ignore it
                                    // this should never happen
                                    if pending.is_empty() {
                                        continue;
                                    }

                                    // If we got here, the task is part of a new potential workflow
                                    // so we to make a copy of the system
                                    let mut new_sys = cur_state.clone();

                                    // Update the state and the workflow
                                    new_sys
                                        .patch(Patch(pending))
                                        .expect("failed to patch system state");
                                    let new_plan = Workflow {
                                        dag,
                                        pending: vec![],
                                    };

                                    // add the new initial state and plan to the stack
                                    stack.push((new_sys, new_plan, depth + 1));
                                }
                                // Ignore harmless errors
                                Err(Error::TaskFailure {
                                    source: task::Error::ConditionFailed,
                                    ..
                                }) => {}
                                Err(Error::LoopDetected) => {}
                                Err(Error::ConditionFailed) => {}
                                Err(Error::NotFound) => {}
                                Err(err) => {
                                    if cfg!(debug_assertions) {
                                        return Err(err);
                                    }
                                    warn!(parent: &parent_span, "{} ... ignoring", err);
                                }
                            }
                        }
                    }
                }
            }
            next_span.record("found", stack.len() - stack_len);
        }

        Err(Error::NotFound)?
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use std::collections::HashMap;

    use super::*;
    use crate::extract::{Args, System, Target, View};
    use crate::task::*;
    use crate::{seq, Dag};

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

    #[test]
    fn it_calculates_a_linear_workflow() {
        let domain = Domain::new().job(update(plus_one)).job(update(minus_one));

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(0, 2).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::plus_one()",
            "gustav::planner::tests::plus_one()"
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_aborts_search_if_plan_length_grows_too_much() {
        let domain = Domain::new()
            .job(update(buggy_plus_one))
            .job(update(minus_one));

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(0, 2);
        assert!(workflow.is_err());
    }

    #[test]
    fn it_calculates_a_linear_workflow_with_compound_tasks() {
        let domain = Domain::new().job(update(plus_two)).job(none(plus_one));

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(0, 2).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::plus_one()",
            "gustav::planner::tests::plus_one()"
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state() {
        #[derive(Serialize)]
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
            .job(("/counters/{counter}", update(minus_one)))
            .job(("/counters/{counter}", update(plus_one)));

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(initial, target).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::plus_one(/counters/two)",
            "gustav::planner::tests::plus_one(/counters/two)",
            "gustav::planner::tests::plus_one(/counters/one)",
            "gustav::planner::tests::plus_one(/counters/one)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state_with_compound_tasks() {
        #[derive(Serialize)]
        struct MyState {
            counters: HashMap<String, i32>,
        }

        let initial = MyState {
            counters: HashMap::from([("one".to_string(), 0), ("two".to_string(), 0)]),
        };

        let target = MyState {
            counters: HashMap::from([("one".to_string(), 2), ("two".to_string(), 0)]),
        };

        let domain = Domain::new().jobs("/counters/{counter}", [none(plus_one), update(plus_two)]);

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(initial, target).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::plus_one(/counters/one)",
            "gustav::planner::tests::plus_one(/counters/one)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn it_calculates_a_linear_workflow_on_a_complex_state_with_deep_compound_tasks() {
        #[derive(Serialize)]
        struct MyState {
            counters: HashMap<String, i32>,
        }

        let initial = MyState {
            counters: HashMap::from([("one".to_string(), 0), ("two".to_string(), 0)]),
        };

        let target = MyState {
            counters: HashMap::from([("one".to_string(), 3), ("two".to_string(), 0)]),
        };

        let domain = Domain::new().jobs(
            "/counters/{counter}",
            [none(plus_one), none(plus_two), update(plus_three)],
        );

        let planner = Planner::new(domain);
        let workflow = planner.find_plan(initial, target).unwrap();

        // We expect a linear DAG with two tasks
        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::plus_one(/counters/one)",
            "gustav::planner::tests::plus_one(/counters/one)",
            "gustav::planner::tests::plus_one(/counters/one)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }

    #[test]
    fn test_stacking_problem() {
        #[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Clone, Debug)]
        enum Block {
            A,
            B,
            C,
        }

        impl From<Block> for String {
            fn from(blk: Block) -> String {
                match blk {
                    Block::A => "A".to_string(),
                    Block::B => "B".to_string(),
                    Block::C => "C".to_string(),
                }
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

        #[derive(Serialize, Deserialize)]
        struct State {
            blocks: Blocks,
        }

        fn is_clear(blocks: &Blocks, loc: &Location) -> bool {
            if loc.is_block() || loc == &Location::Hand {
                // No block is on top of the location
                return blocks.iter().all(|(_, l)| l != loc);
            }
            true
        }

        fn is_holding(blocks: &Blocks) -> bool {
            !is_clear(blocks, &Location::Hand)
        }

        fn all_clear(blocks: &Blocks) -> Vec<&Block> {
            blocks
                .iter()
                .filter(|(_, l)| is_clear(blocks, l))
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
        ) -> View<Location> {
            // if the block is clear and we are not holding any other blocks
            // we can grab the block
            if loc.is_block()
                && is_clear(&sys.blocks, &Location::Blk(block))
                && !is_holding(&sys.blocks)
            {
                *loc = Location::Hand;
            }

            loc
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

        fn take(loc: View<Location>, System(sys): System<State>) -> Vec<Task> {
            if is_clear(&sys.blocks, &loc) {
                if *loc == Location::Table {
                    return vec![pickup.into_task()];
                } else {
                    return vec![unstack.into_task()];
                }
            }
            vec![]
        }

        fn put(loc: View<Location>, Target(tgt): Target<Location>) -> Vec<Task> {
            if *loc == Location::Hand {
                if tgt == Location::Table {
                    return vec![putdown.into_task()];
                } else {
                    return vec![stack.with_target(tgt)];
                }
            }
            vec![]
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
            for b in all_clear(&blocks) {
                // we assume that the target is well formed
                let tgt_blk = target.get(b).unwrap();

                // The block is free and it can be moved to the final target (another block or the table)
                if is_clear(&blocks, tgt_blk) {
                    return vec![
                        take.with_arg("block", b.clone()),
                        put.with_target(tgt_blk).with_arg("block", b.clone()),
                    ];
                }
            }

            // If we get here, no blocks can be moved to the final location so
            // we move it to the table
            let mut to_table: Vec<Task> = vec![];
            for b in all_clear(&blocks) {
                to_table.push(take.with_arg("block", b.clone()));
                to_table.push(
                    put.with_target(Location::Table)
                        .with_arg("block", b.clone()),
                );
            }

            to_table
        }
        let domain = Domain::new()
            .jobs(
                "/blocks/{block}",
                [
                    update(pickup),
                    update(unstack),
                    update(putdown),
                    update(stack),
                    update(take),
                    update(put),
                ],
            )
            .job(("/blocks", update(move_blks)));

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

        let workflow = planner.find_plan(initial, target).unwrap();

        let expected: Dag<&str> = seq!(
            "gustav::planner::tests::test_stacking_problem::unstack(/blocks/C)",
            "gustav::planner::tests::test_stacking_problem::stack(/blocks/C)",
            "gustav::planner::tests::test_stacking_problem::unstack(/blocks/B)",
            "gustav::planner::tests::test_stacking_problem::stack(/blocks/B)",
            "gustav::planner::tests::test_stacking_problem::pickup(/blocks/A)",
            "gustav::planner::tests::test_stacking_problem::stack(/blocks/A)",
        );

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }
}

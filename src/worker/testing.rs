use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;

use super::{Idle, Ready, Worker};
use crate::planner::{Error as PlannerError, Planner};
use crate::system::System;
use crate::task::{self, Context};
use crate::{task::Task, workflow::Workflow};

#[derive(Debug, Error)]
#[error("workflow not found")]
pub struct NotFound;

pub async fn find_workflow<I>(
    system: &Arc<RwLock<System>>,
    planner: &Planner,
    tgt: I,
) -> Result<Workflow, NotFound>
where
    I: Serialize + DeserializeOwned,
{
    let cur = {
        let sys = system.read().await;
        sys.clone()
    };

    let tgt = serde_json::to_value(tgt).expect("failed to serialize target state");

    match planner.find_workflow::<I>(&cur, &tgt) {
        Ok(workflow) => Ok(workflow),
        Err(PlannerError::NotFound) => Err(NotFound),
        Err(e) => panic!("unexpected planning error: {e}"),
    }
}

impl<O, I> Worker<O, Ready, I> {
    /// Find a workflow within the context of the worker
    ///
    /// This function is only meant for testing and is not available
    /// if debug_assertions is disabled.
    ///
    /// # Example
    /// ```rust
    /// use mahler::task::{self, prelude::*};
    /// use mahler::extract::{View, Target};
    /// use mahler::worker::Worker;
    /// use mahler::{Dag, seq};
    ///
    /// fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> IO<i32> {
    ///    if *counter < tgt {
    ///        // Modify the counter if we are below target
    ///        *counter += 1;
    ///    }
    ///
    ///    // Return the updated counter
    ///    with_io(counter, |counter| async {
    ///        Ok(counter)
    ///    })
    /// }
    ///
    /// # tokio_test::block_on(async {
    /// // Setup the worker domain and resources
    /// let worker = Worker::new()
    ///                 .job("", update(plus_one).with_description(|| "+1"))
    ///                 .initial_state(0)
    ///                 .unwrap();
    /// let workflow = worker.find_workflow(2).await.unwrap();
    ///
    /// // We expect a linear DAG with two tasks
    /// let expected: Dag<&str> = seq!("+1", "+1");
    /// assert_eq!(workflow.to_string(), expected.to_string());
    /// # })
    /// ```
    pub async fn find_workflow(&self, tgt: I) -> Result<Workflow, NotFound>
    where
        I: Serialize + DeserializeOwned,
    {
        find_workflow::<I>(&self.inner.system, &self.inner.planner, tgt).await
    }

    async fn run_task_with_system(
        &self,
        mut task: Task,
        system: &mut System,
    ) -> Result<(), task::Error> {
        let task_id = task.id().to_string();
        let Context { args, .. } = task.context_mut();
        let path = self
            .inner
            .planner
            .domain()
            .find_path_for_job(task_id.as_str(), args)
            .expect("could not find path for task");

        let task = task.with_path(path);
        match &task {
            Task::Action(action) => {
                let changes = action.run(system).await?;
                system
                    .patch(changes)
                    .expect("failed to patch the system state");
            }
            Task::Method(method) => {
                let tasks = method.expand(system)?;
                for mut task in tasks {
                    // Propagate the parent args to the child task
                    for (k, v) in method.context().args.iter() {
                        task = task.with_arg(k, v)
                    }
                    Box::pin(self.run_task_with_system(task, system)).await?;
                }
            }
        }

        Ok(())
    }

    /// Test a task within the context of the worker domain
    ///
    /// This function is only meant for testing and is not available
    /// if debug_assertions is disabled.
    ///
    /// # Example
    /// ```rust
    /// use std::time::Duration;
    /// use tokio::time::sleep;
    ///
    /// use mahler::task::{self, prelude::*};
    /// use mahler::extract::{View, Target};
    /// use mahler::worker::Worker;
    ///
    /// fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> IO<i32> {
    ///    if *counter < tgt {
    ///        // Modify the counter if we are below target
    ///        *counter += 1;
    ///    }
    ///
    ///    // Return the updated counter
    ///    with_io(counter, |counter| async {
    ///        sleep(Duration::from_millis(10)).await;
    ///        Ok(counter)
    ///    })
    /// }
    ///
    /// # tokio_test::block_on(async {
    /// // Setup the worker domain and resources
    /// let worker = Worker::new().job("", update(plus_one)).initial_state::<i32>(0).unwrap();
    ///
    /// // Run task emulating a target of 2 and initial state of 0
    /// assert_eq!(worker.run_task(plus_one.with_target(2)).await.unwrap(), 1);
    ///
    /// // Run task emulating a target of 2 and initial state of 2 (no changes)
    /// let worker = Worker::new().job("", update(plus_one)).initial_state::<i32>(2).unwrap();
    /// assert_eq!(worker.run_task(plus_one.with_target(2)).await.unwrap(), 2);
    /// # })
    /// ```
    pub async fn run_task(&self, mut task: Task) -> Result<O, task::Error>
    where
        O: Serialize + DeserializeOwned,
    {
        let mut system = self.inner.system.read().await.clone();
        let task_id = task.id().to_string();
        let Context { args, .. } = task.context_mut();
        let path = self
            .inner
            .planner
            .domain()
            .find_path_for_job(task_id.as_str(), args)
            .expect("could not find path for task");

        let task = task.with_path(path);
        self.run_task_with_system(task, &mut system).await?;

        let new_state = system.state().expect("failed to serialize output state");

        Ok(new_state)
    }
}

impl<O, I> Worker<O, Idle, I> {
    pub async fn find_workflow(&self, tgt: I) -> Result<Workflow, NotFound>
    where
        I: Serialize + DeserializeOwned,
    {
        find_workflow::<I>(&self.inner.system, &self.inner.planner, tgt).await
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use std::collections::HashMap;

    use super::*;
    use crate::extract::{Target, View};
    use crate::{par, seq, task::*, Dag};

    fn plus_one(mut counter: View<i32>, tgt: Target<i32>) -> View<i32> {
        if *counter < *tgt {
            *counter += 1;
        }

        // Update implements IntoResult
        counter
    }

    fn plus_two(counter: View<i32>, tgt: Target<i32>) -> Vec<Task> {
        if *tgt - *counter < 2 {
            // Returning an empty result tells the planner
            // the task is not applicable to reach the target
            return vec![];
        }

        vec![plus_one.with_target(*tgt), plus_one.with_target(*tgt)]
    }

    #[tokio::test]
    async fn it_allows_testing_atomic_tasks() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Counters(HashMap<String, i32>);

        let worker = Worker::new()
            .job("/{counter}", update(plus_one))
            .job("/{counter}", update(plus_two))
            .initial_state::<Counters>(Counters(HashMap::from([
                ("one".to_string(), 1),
                ("two".to_string(), 0),
            ])))
            .unwrap();

        let task = plus_one.with_target(3).with_arg("counter", "one");
        let res = worker.run_task(task).await.unwrap();

        assert_eq!(
            res,
            Counters(HashMap::from([
                ("one".to_string(), 2),
                ("two".to_string(), 0),
            ]))
        );
    }

    #[tokio::test]
    async fn it_allows_testing_compound_tasks() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Counters(HashMap<String, i32>);

        let worker = Worker::new()
            .job("/{counter}", update(plus_one))
            .job("/{counter}", update(plus_two))
            .initial_state::<Counters>(Counters(HashMap::from([
                ("one".to_string(), 0),
                ("two".to_string(), 0),
            ])))
            .unwrap();

        let task = plus_two.with_target(3).with_arg("counter", "one");

        let res = worker.run_task(task).await.unwrap();

        assert_eq!(
            res,
            Counters(HashMap::from([
                ("one".to_string(), 2),
                ("two".to_string(), 0),
            ]))
        );
    }

    #[tokio::test]
    async fn it_allows_searching_for_workflow() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Counters(HashMap<String, i32>);

        let worker = Worker::new()
            .job("/{counter}", update(plus_one))
            .job("/{counter}", update(plus_two))
            .initial_state(Counters(HashMap::from([
                ("one".to_string(), 0),
                ("two".to_string(), 0),
            ])))
            .unwrap();

        let workflow = worker
            .find_workflow(Counters(HashMap::from([
                ("one".to_string(), 2),
                ("two".to_string(), 1),
            ])))
            .await
            .unwrap();

        // We expect a linear DAG with three tasks
        let expected: Dag<&str> = par!(
            "mahler::worker::testing::tests::plus_one(/one)",
            "mahler::worker::testing::tests::plus_one(/two)"
        ) + seq!("mahler::worker::testing::tests::plus_one(/one)",);

        assert_eq!(workflow.to_string(), expected.to_string(),);
    }
}

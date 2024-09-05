mod action;
mod boxed;
mod effect;
mod method;
mod result;

use json_patch::{Patch, PatchOperation};

use crate::error::Error;
use crate::system::{Context, System};

use action::ActionResult;
use boxed::*;
use effect::{Effect, IntoAction};

pub use action::Action;
pub use method::Method;
pub(crate) use result::*;

pub struct Job<S> {
    builder: BoxedIntoTask<S>,
}

impl<S> Job<S> {
    fn new<E, A, T>(effect: E, action: A) -> Self
    where
        E: Effect<S, T>,
        A: Action<S, T>,
        S: 'static,
    {
        Self {
            builder: BoxedIntoTask::from_unit(effect, action),
        }
    }

    pub fn from<E, T>(effect: E) -> Self
    where
        E: Effect<S, T> + 'static,
        T: Send + 'static,
        S: Send + Sync + 'static,
    {
        Self::new(effect.clone(), IntoAction::new(effect))
    }

    pub fn bind(&self, context: Context<S>) -> Task<S> {
        self.builder.clone().into_task(context)
    }
}

type DryRun<S> = Box<dyn FnOnce(&System, Context<S>) -> Result>;
type Run<S> = Box<dyn FnOnce(&System, Context<S>) -> ActionResult>;
type Expand<S> = Box<dyn FnOnce(&System, Context<S>) -> core::result::Result<Vec<Task<S>>, Error>>;

pub enum Task<S> {
    Unit {
        context: Context<S>,
        dry_run: DryRun<S>,
        run: Run<S>,
    },
    Group {
        context: Context<S>,
        expand: Expand<S>,
    },
}

impl<S> Task<S> {
    pub(crate) fn unit<E, H, T>(effect: E, handler: H, context: Context<S>) -> Self
    where
        E: Effect<S, T>,
        H: Action<S, T>,
    {
        Self::Unit {
            context,
            dry_run: Box::new(|system: &System, context: Context<S>| {
                effect.call(system.clone(), context)
            }),
            run: Box::new(|system: &System, context: Context<S>| {
                Box::pin(handler.call(system.clone(), context))
            }),
        }
    }

    /// Run every action in the job sequentially and return the
    /// aggregate changes.
    pub fn dry_run(self, system: &System) -> Result {
        match self {
            Self::Unit {
                context, dry_run, ..
            } => (dry_run)(system, context),
            Self::Group { context, expand } => {
                let mut changes: Vec<PatchOperation> = vec![];
                let jobs = (expand)(system, context)?;
                for job in jobs {
                    job.dry_run(system)
                        .map(|Patch(mut patch)| changes.append(&mut patch))?;
                }
                Ok(Patch(changes))
            }
        }
    }

    /// Run the job sequentially
    pub async fn run(self, system: &mut System) -> core::result::Result<(), Error> {
        match self {
            Self::Unit { context, run, .. } => {
                let changes = (run)(system, context).await?;
                system.patch(changes)
            }
            // NOTE: This is only implemented for testing purposes, workflows returned
            // by a planner will only include unit tasks as the workflow defines whether
            // the tasks can be executed concurrently or in parallel
            Self::Group {
                context, expand, ..
            } => {
                let jobs = (expand)(system, context)?;
                for job in jobs {
                    Box::pin(job.run(system)).await?;
                }
                Ok(())
            }
        }
    }

    /// Expand the job into its composing sub-jobs.
    ///
    /// If the job is a Unit, it will return an empty vector
    pub fn expand(self, system: &System) -> core::result::Result<Vec<Task<S>>, Error> {
        match self {
            Self::Unit { .. } => Ok(vec![]),
            Self::Group {
                context, expand, ..
            } => (expand)(system, context),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Target, Update};
    use crate::system::{Context, System};
    use json_patch::Patch;
    use serde::{Deserialize, Serialize};
    use serde_json::{from_value, json};
    use std::collections::HashMap;
    use thiserror::Error;

    #[derive(Error, Debug)]
    enum MyError {
        #[error("counter already reached")]
        CounterReached,
    }

    fn my_task_effect(mut counter: Update<i32>, tgt: Target<i32>) -> impl IntoResult {
        if *counter < *tgt {
            *counter += 1;
        }

        // Update implements IntoResult
        counter
    }

    async fn my_task_action(mut counter: Update<i32>, tgt: Target<i32>) -> impl IntoResult {
        if *counter < *tgt {
            *counter += 1;
        } else {
            return Err(MyError::CounterReached);
        }

        Ok(counter)
    }

    #[test]
    fn it_allows_to_dry_run_tasks() {
        let system = System::from(0);
        let task = Job::from(my_task_effect);
        let action = task.bind(Context::from(1));

        // Get the list of changes that the action performs
        let changes = action.dry_run(&system).unwrap();
        assert_eq!(
            changes,
            from_value::<Patch>(json!([
              { "op": "replace", "path": "", "value": 1 },
            ]))
            .unwrap()
        );
    }

    #[tokio::test]
    async fn it_runs_async_actions() {
        let mut system = System::from(1);
        let task = Job::from(my_task_effect);
        let action = task.bind(Context::from(1));

        // Run the action
        action.run(&mut system).await.unwrap();

        let state = system.state::<i32>().unwrap();

        // The referenced value was modified
        assert_eq!(state, 1);
    }

    #[tokio::test]
    async fn it_allows_extending_actions_with_effect() {
        let mut system = System::from(0);
        let task = my_task_action.with_effect(my_task_effect);
        let action = task.bind(Context::from(1));

        // Run the action
        action.run(&mut system).await.unwrap();

        // Check that the system state was modified
        let state = system.state::<i32>().unwrap();
        assert_eq!(state, 1);
    }

    #[tokio::test]
    async fn it_allows_actions_returning_errors() {
        let mut system = System::from(1);
        let task = my_task_action.with_effect(my_task_effect);
        let action = task.bind(Context::from(1));

        let res = action.run(&mut system).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().to_string(), "counter already reached");
    }

    // State needs to be clone in order for Target to implement IntoSystem
    #[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
    struct State {
        counters: HashMap<String, i32>,
    }

    fn update_counter(
        mut counter: Update<State, i32>,
        tgt: Target<State, i32>,
    ) -> Update<State, i32> {
        if *counter < *tgt {
            *counter += 1;
        }

        // Update implements IntoResult
        counter
    }

    #[tokio::test]
    async fn it_modifies_system_sub_elements() {
        let state = State {
            counters: [("a".to_string(), 0), ("b".to_string(), 0)].into(),
        };

        let mut system = System::from(state);
        let task = Job::from(update_counter);
        let action = task.bind(
            Context::from(State {
                counters: [("a".to_string(), 2), ("b".to_string(), 1)].into(),
            })
            .with_path("/counters/a"),
        );

        // Run the action
        action.run(&mut system).await.unwrap();

        let state = system.state::<State>().unwrap();

        // Only the referenced value was modified
        assert_eq!(
            state,
            State {
                counters: [("a".to_string(), 1), ("b".to_string(), 0)].into()
            }
        );
    }
}

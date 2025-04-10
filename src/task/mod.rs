mod context;
mod description;
mod effect;
mod errors;
mod handler;
mod into_result;
mod job;
mod with_io;

use anyhow::anyhow;
use anyhow::Context as AnyhowCtx;
use json_patch::Patch;
use serde::Serialize;
use std::fmt::{self, Display};
use std::future::Future;
use std::panic;
use std::panic::RefUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use tracing::warn;

use crate::system::System;

pub(crate) use context::*;
pub(crate) use errors::*;
pub(crate) use into_result::*;

pub use description::*;
pub use effect::*;
pub use errors::InputError;
pub use handler::*;
pub use job::*;
pub use with_io::*;

pub mod prelude {
    pub use super::handler::*;
    pub use super::job::{any, create, delete, none, update};
    pub use super::with_io::*;
}

type ActionOutput = Pin<Box<dyn Future<Output = Result<Patch, Error>> + Send>>;
type DryRun = Arc<dyn Fn(&System, &Context) -> Result<Patch, Error> + Send + Sync>;
type Run = Arc<dyn Fn(&System, &Context) -> ActionOutput + Send + Sync>;
type Expand = Arc<dyn Fn(&System, &Context) -> Result<Vec<Task>, Error> + Send + Sync>;
type Describe = Arc<dyn Fn(&Context) -> Result<String, Error> + Send + Sync>;

#[derive(Clone)]
pub struct Action {
    id: &'static str,
    scoped: bool,
    context: Context,
    dry_run: DryRun,
    run: Run,
    describe: Describe,
}

impl fmt::Debug for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Action<'a> {
            id: &'a str,
            scoped: &'a bool,
            context: &'a Context,
        }

        let Self {
            id,
            scoped,
            context,
            ..
        } = self;
        fmt::Debug::fmt(
            &Action {
                id,
                scoped,
                context,
            },
            f,
        )
    }
}

impl Action {
    pub(crate) fn new<H, T, I>(action: H, context: Context) -> Self
    where
        H: Handler<T, Patch, I>,
        I: Send + 'static,
    {
        let handler_clone = action.clone();
        Self {
            id: action.id(),
            scoped: action.is_scoped(),
            context,
            dry_run: Arc::new(move |system: &System, context: &Context| {
                let effect = handler_clone.call(system, context);
                effect.pure()
            }),
            run: Arc::new(move |system: &System, context: &Context| {
                let effect = action.call(system, context);

                Box::pin(async { effect.run().await })
            }),
            describe: Arc::new(|_: &Context| {
                // This error should never be seen
                Err(UnexpectedError::from(anyhow!("undefined description")))?
            }),
        }
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    pub fn id(&self) -> &str {
        self.id
    }

    /// Run the task sequentially
    pub(crate) async fn run(&self, system: &System) -> Result<Patch, Error> {
        let Action { context, run, .. } = self;
        (run)(system, context).await
    }

    pub(crate) fn dry_run(&self, system: &System) -> Result<Patch, Error> {
        let Action {
            context, dry_run, ..
        } = self;
        (dry_run)(system, context)
    }
}

impl Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = (self.describe)(self.context()).unwrap_or_else(|e| {
            match e {
                Error::Unexpected(_) => {}
                _ => warn!("failed to expand description for task {}: {}", self.id, e),
            }
            format!("{}({})", self.id, self.context.path)
        });
        write!(f, "{}", description)
    }
}

#[derive(Clone)]
pub struct Method {
    id: &'static str,
    scoped: bool,
    context: Context,
    expand: Expand,
    describe: Describe,
}

impl fmt::Debug for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Method<'a> {
            id: &'a str,
            scoped: &'a bool,
            context: &'a Context,
        }

        let Self {
            id,
            scoped,
            context,
            ..
        } = self;
        fmt::Debug::fmt(
            &Method {
                id,
                scoped,
                context,
            },
            f,
        )
    }
}

impl Method {
    pub(crate) fn new<H, T>(method: H, context: Context) -> Self
    where
        H: Handler<T, Vec<Task>> + RefUnwindSafe,
    {
        Method {
            id: method.id(),
            scoped: method.is_scoped(),
            context,
            expand: Arc::new(move |system: &System, context: &Context| {
                // Because with_target has a chance of panicking, we catch any panics
                // on task methods to simplify things for library users
                panic::catch_unwind(|| method.call(system, context))
                    .map(|r| r.pure())
                    .map_err(|e| {
                        let msg = e
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                            .or_else(|| e.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| {
                                format!("Panic while expanding task {}", method.id())
                            });

                        InputError::from(anyhow!(msg))
                    })?
            }),
            describe: Arc::new(|_: &Context| {
                // This error should never be seen
                Err(UnexpectedError::from(anyhow!("undefined description")))?
            }),
        }
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    pub fn id(&self) -> &str {
        self.id
    }

    pub(crate) fn expand(&self, system: &System) -> Result<Vec<Task>, Error> {
        let Method {
            context, expand, ..
        } = self;
        (expand)(system, context)
    }
}

impl Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = (self.describe)(self.context()).unwrap_or_else(|e| {
            match e {
                Error::Unexpected(_) => {}
                _ => warn!("failed to expand description for task {}: {}", self.id, e),
            }
            format!("{}({})", self.id, self.context.path)
        });
        write!(f, "{}", description)
    }
}

/// The Task degree denotes its cardinality or its position in a search tree
///
/// - Atom jobs are the leafs in the search tree, they define the work to be
///   executed and cannot be expanded
/// - List jobs define work in terms of other tasks, they are expanded recursively
///   in order to get to a list of atoms
#[derive(Clone, PartialEq, PartialOrd, Eq, Ord, Debug)]
pub(crate) enum Degree {
    Composite,
    Atomic,
}

/// A task is either a concrete unit of work (action) or rule to
/// derive a list of tasks (method)
#[derive(Debug, Clone)]
pub enum Task {
    Action(Action),
    Method(Method),
}

impl From<Action> for Task {
    fn from(action: Action) -> Self {
        Self::Action(action)
    }
}

impl From<Method> for Task {
    fn from(method: Method) -> Self {
        Self::Method(method)
    }
}

impl Task {
    /// Get the unique identifier for the task
    pub fn id(&self) -> &str {
        match self {
            Self::Action(Action { id, .. }) => id,
            Self::Method(Method { id, .. }) => id,
        }
    }

    pub(crate) fn degree(&self) -> Degree {
        match self {
            Self::Action(_) => Degree::Atomic,
            Self::Method(_) => Degree::Composite,
        }
    }

    pub(crate) fn context(&self) -> &Context {
        match self {
            Self::Action(Action { context, .. }) => context,
            Self::Method(Method { context, .. }) => context,
        }
    }

    /// Return true if the task can be parallelized
    pub fn is_scoped(&self) -> bool {
        match self {
            Self::Action(Action { scoped, .. }) => *scoped,
            Self::Method(Method { scoped, .. }) => *scoped,
        }
    }

    /// Set a target for the task
    ///
    /// This returns a result with an error if the serialization of the target fails
    pub fn try_target<S: Serialize>(self, target: S) -> Result<Self, InputError> {
        let target = serde_json::to_value(target).context("Serialization failed")?;
        Ok(match self {
            Self::Action(task) => {
                let Action { context, .. } = task;
                Self::Action(Action {
                    context: context.with_target(target),
                    ..task
                })
            }
            Self::Method(task) => {
                let Method { context, .. } = task;
                Self::Method(Method {
                    context: context.with_target(target),
                    ..task
                })
            }
        })
    }

    /// Set a target for the task
    ///
    /// This function will panic if the serialization of the target fails
    pub fn with_target<S: Serialize>(self, target: S) -> Self {
        self.try_target(target).unwrap()
    }

    /// Set an argument for the task
    pub fn with_arg(self, key: impl AsRef<str>, value: impl Into<String>) -> Self {
        match self {
            Self::Action(task) => {
                let Action { context, .. } = task;
                Self::Action(Action {
                    context: context.with_arg(key, value),
                    ..task
                })
            }
            Self::Method(task) => {
                let Method { context, .. } = task;
                Self::Method(Method {
                    context: context.with_arg(key, value),
                    ..task
                })
            }
        }
    }

    pub(crate) fn with_path(self, path: impl AsRef<str>) -> Self {
        match self {
            Self::Action(task) => {
                let Action { context, .. } = task;
                Self::Action(Action {
                    context: context.with_path(path),
                    ..task
                })
            }
            Self::Method(task) => {
                let Method { context, .. } = task;
                Self::Method(Method {
                    context: context.with_path(path),
                    ..task
                })
            }
        }
    }

    pub(crate) fn with_context(self, context: Context) -> Self {
        match self {
            Self::Action(task) => Self::Action(Action { context, ..task }),
            Self::Method(task) => Self::Method(Method { context, ..task }),
        }
    }

    pub(crate) fn with_description<D, T>(self, description: D) -> Self
    where
        D: Description<T>,
    {
        let describe: Describe = Arc::new(move |ctx| description.call(ctx));
        match self {
            Self::Action(task) => Self::Action(Action { describe, ..task }),
            Self::Method(task) => Self::Method(Method { describe, ..task }),
        }
    }
}

impl Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Action(action) => action.fmt(f),
            Self::Method(method) => method.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{System as Sys, Target, View};
    use crate::system::System;
    use pretty_assertions::assert_eq;
    use serde::{Deserialize, Serialize};
    use serde_json::{from_value, json};
    use std::collections::{BTreeMap, HashMap};
    use thiserror::Error;
    use tokio::time::{sleep, Duration};

    #[derive(Error, Debug)]
    #[error("and error happened")]
    struct SomeError;

    fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> View<i32> {
        if *counter < tgt {
            *counter += 1;
        }

        // Update implements IntoResult
        counter
    }

    fn plus_one_async_with_effect(
        mut counter: View<i32>,
        Target(tgt): Target<i32>,
    ) -> Effect<View<i32>> {
        if *counter < tgt {
            *counter += 1;
        }

        Effect::of(counter).with_io(|counter| async {
            sleep(Duration::from_millis(10)).await;
            Ok(counter)
        })
    }

    fn plus_one_async_with_error(
        mut counter: View<i32>,
        Target(tgt): Target<i32>,
    ) -> Effect<View<i32>, SomeError> {
        if *counter < tgt {
            *counter += 1;
        }

        Effect::of(counter).with_io(|_| async {
            sleep(Duration::from_millis(10)).await;
            Err(SomeError)
        })
    }

    #[test]
    fn it_gets_metadata_from_function() {
        assert_eq!(plus_one.id(), "gustav::task::tests::plus_one");
    }

    #[test]
    fn it_allows_to_describe_a_task() {
        let task = plus_one.into_task().with_description(|| "+1");
        assert_eq!(task.to_string(), "+1");

        let task = plus_one
            .into_task()
            .with_description(|Target(tgt): Target<i32>| format!("+1 until {tgt}"));

        // The target has not been assigned so the default description is returned
        assert_eq!(task.to_string(), "gustav::task::tests::plus_one()");

        let task = task.with_target(2);
        // Now the description can be used
        assert_eq!(task.to_string(), "+1 until 2");
    }

    #[test]
    fn it_identifies_task_scoping_based_on_args() {
        let task = plus_one.with_target(1);
        assert!(task.is_scoped());

        #[derive(Serialize, Deserialize, Debug)]
        struct State {
            numbers: HashMap<String, i32>,
        }

        fn plus_one_sys(
            mut counter: View<i32>,
            Target(tgt): Target<i32>,
            Sys(_): Sys<State>,
        ) -> View<i32> {
            if *counter < tgt {
                *counter += 1;
            }

            // Update implements IntoResult
            counter
        }

        // The plus_one_sys uses the System extractor so
        // it is not scoped
        let task = plus_one_sys.with_target(1);
        assert!(!task.is_scoped());
    }

    #[tokio::test]
    async fn it_runs_async_actions() {
        let system = System::try_from(0).unwrap();
        let task = plus_one.with_target(1);

        if let Task::Action(action) = task {
            // Run the action
            let changes = action.run(&system).await.unwrap();

            // The referenced value was modified
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "", "value": 1 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    #[tokio::test]
    async fn it_allows_extending_actions_with_effect() {
        let system = System::try_from(0).unwrap();
        let task = plus_one_async_with_effect.with_target(1);

        if let Task::Action(action) = task {
            // Run the action
            let changes = action.run(&system).await.unwrap();

            // The referenced value was modified
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "", "value": 1 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    #[tokio::test]
    async fn it_allows_actions_returning_runtime_errors() {
        let system = System::try_from(0).unwrap();
        let task = plus_one_async_with_error.with_target(1);

        if let Task::Action(action) = task {
            let res = action.run(&system).await;
            assert!(res.is_err());
            assert_eq!(
                res.unwrap_err().to_string(),
                "task runtime error: and error happened"
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    #[test]
    fn it_allows_to_dry_run_actions_returning_error() {
        let system = System::try_from(1).unwrap();
        let task = plus_one_async_with_error.with_target(2);

        if let Task::Action(action) = task {
            let changes = action.dry_run(&system).unwrap();
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "", "value": 2 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    #[test]
    fn it_allows_to_dry_run_pure_actions() {
        let system = System::try_from(1).unwrap();
        let task = plus_one.with_target(2);

        if let Task::Action(action) = task {
            let changes = action.dry_run(&system).unwrap();
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "", "value": 2 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    #[test]
    fn it_allows_to_dry_run_async_actions() {
        let system = System::try_from(1).unwrap();
        let task = plus_one_async_with_effect.with_target(2);

        if let Task::Action(action) = task {
            let changes = action.dry_run(&system).unwrap();
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "", "value": 2 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action task");
        }
    }

    // State needs to be clone in order for Target to implement IntoSystem
    #[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
    struct State {
        counters: HashMap<String, i32>,
    }

    fn update_counter(mut counter: View<i32>, tgt: Target<i32>) -> View<i32> {
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

        let system = System::try_from(state).unwrap();
        let task = update_counter.with_target(2).with_path("/counters/a");

        if let Task::Action(action) = task {
            // Run the action
            let changes = action.run(&system).await.unwrap();

            // Only the referenced value was modified
            assert_eq!(
                changes,
                from_value::<Patch>(json!([
                  { "op": "replace", "path": "/counters/a", "value": 1 },
                ]))
                .unwrap()
            );
        } else {
            panic!("Expected an Action Task");
        }
    }

    fn plus_two_with_error(
        counter: View<i32>,
        Target(tgt): Target<i32>,
    ) -> Result<[Task; 2], Error> {
        if tgt - *counter > 1 {
            return Ok([
                plus_one.into_task().try_target(tgt)?,
                plus_one.into_task().try_target(tgt)?,
            ]);
        }

        Err(Error::ConditionFailed)
    }

    #[test]
    fn it_allows_expanding_methods_with_result() {
        let task = plus_two_with_error.with_target(3);
        let system = System::try_from(0).unwrap();

        if let Task::Method(method) = task {
            let tasks = method.expand(&system).unwrap();
            assert_eq!(
                tasks.iter().map(|t| t.id()).collect::<Vec<&str>>(),
                vec![plus_one.id(), plus_one.id()]
            );
        } else {
            panic!("Expected a method task");
        }
    }

    #[test]
    fn it_catches_input_errors_in_method_expansions() {
        let task = plus_two_with_error.with_target("a");
        let system = System::try_from(0).unwrap();

        if let Task::Method(method) = task {
            assert!(matches!(method.expand(&system), Err(Error::BadInput(_))));
        } else {
            panic!("Expected a method task");
        }
    }

    #[test]
    fn it_catches_condition_failure_in_method_expansions() {
        let task = plus_two_with_error.with_target(1);
        let system = System::try_from(0).unwrap();

        if let Task::Method(method) = task {
            assert!(matches!(
                method.expand(&system),
                Err(Error::ConditionFailed)
            ));
        } else {
            panic!("Expected a method task");
        }
    }

    fn plus_two_with_panic(counter: View<i32>, Target(tgt): Target<i32>) -> Option<[Task; 2]> {
        if tgt - *counter > 1 {
            // Force `with_target` to panic
            // See: https://docs.rs/serde_json/latest/serde_json/fn.to_value.html#errors
            let mut map = BTreeMap::new();
            map.insert(vec![32, 64], "x86");
            return Some([
                plus_one.into_task().with_target(map),
                plus_one.into_task().with_target(tgt),
            ]);
        }

        None
    }

    #[test]
    fn it_catches_panics_in_method_expansions() {
        // this will cause the
        let task = plus_two_with_panic.with_target(3);
        let system = System::try_from(0).unwrap();

        if let Task::Method(method) = task {
            assert!(matches!(method.expand(&system), Err(Error::BadInput(_))));
        } else {
            panic!("Expected a method task");
        }
    }

    #[test]
    fn it_catches_condition_failure_in_methods_returning_option() {
        let task = plus_two_with_panic.with_target(1);
        let system = System::try_from(0).unwrap();

        if let Task::Method(method) = task {
            assert!(matches!(
                method.expand(&system),
                Err(Error::ConditionFailed)
            ));
        } else {
            panic!("Expected a method task");
        }
    }
}

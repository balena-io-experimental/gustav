use anyhow::Context as AnyhowCtx;
use serde::de::DeserializeOwned;
use std::ops::Deref;

use crate::errors::ExtractionError;
use crate::system::System;
use crate::task::{Context, FromContext, FromSystem};

mod de;
mod error;

#[derive(Debug)]
/// Extracts arguments from the Task path and parses them using [`serde`]
///
/// # Example
///
/// One `Args` can extract multiple arguments. A handler should not be given more than one
/// `Args` argument.
///
/// ```rust,no_run
/// use mahler::{
///     extract::Args,
///     task::{Handler, update},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn install_service_for_release(Args((release_id, service_name)): Args<(String, String)>) {
///     // ...
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/releases/{release_id}/services/{service_name}", update(install_service_for_release))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// # Errors
///
/// If the path arguments cannot be deserialized into the target type, extraction will fail and an
/// error will be logged by the [Worker](`crate::worker::Worker`).
pub struct Args<T>(pub T);

impl<T: DeserializeOwned + Send> FromContext for Args<T> {
    type Error = ExtractionError;

    fn from_context(context: &Context) -> Result<Self, Self::Error> {
        let args = &context.args;
        let value = T::deserialize(de::PathDeserializer::new(args)).with_context(|| {
            format!(
                "Failed to deserialize {args} into {}",
                std::any::type_name::<T>()
            )
        })?;

        Ok(Args(value))
    }
}

impl<T: DeserializeOwned + Send> FromSystem for Args<T> {
    type Error = ExtractionError;

    fn from_system(_: &System, context: &Context) -> Result<Self, Self::Error> {
        Self::from_context(context)
    }
}

impl<S> Deref for Args<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    // The state model
    #[derive(Serialize, Deserialize, Debug, Clone)]
    struct State {
        numbers: HashMap<String, i32>,
    }

    #[test]
    fn deserializes_simple_path_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let Args(name): Args<String> =
            Args::from_system(&system, &Context::new().with_arg("name", "one")).unwrap();

        assert_eq!(name, "one");
    }

    #[test]
    fn deserializes_tuple_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let Args((first, second)): Args<(String, String)> = Args::from_system(
            &system,
            &Context::new()
                .with_arg("first", "one")
                .with_arg("second", "two"),
        )
        .unwrap();

        assert_eq!(first, "one");
        assert_eq!(second, "two");
    }

    #[test]
    fn deserializes_hashmap_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let Args(map): Args<HashMap<String, String>> = Args::from_system(
            &system,
            &Context::new()
                .with_arg("first", "one")
                .with_arg("second", "two"),
        )
        .unwrap();

        assert_eq!(
            map,
            HashMap::from([
                ("first".to_string(), "one".to_string()),
                ("second".to_string(), "two".to_string())
            ])
        );
    }
}

use anyhow::Context as AnyhowCxt;
use serde::de::DeserializeOwned;
use std::ops::Deref;

use crate::errors::ExtractionError;
use crate::system::System;
use crate::task::{Context, FromContext, FromSystem};

#[derive(Debug)]
/// Extracts the target state for tasks created from the job handler
///
/// Allows the Job to see the desired state for the path assigned to the task.
///
/// # Example
///
/// ```rust,no_run
/// use mahler::{
///     extract::Target,
///     task::{Handler, update},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn with_target(Target(tgt): Target<i32>) {
///     // ...
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", update(with_target))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// # Errors
///
/// Initializing the extractor will fail if the target value for the task cannot be deserialized
/// into the given type `<T>`. This includes the case where no target exists, for instance, for
/// `delete` type jobs.
///
/// ```rust,no_run
/// use mahler::{
///     extract::Target,
///     task::{Handler, delete},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// // this task is applicable to `delete` operations which do not
/// // have a target, thus this extractor will always fail to be initialized
/// fn invalid_task(Target(tgt): Target<i32>) {
///     // ...
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     // `invalid_task` will be called if a delete operation on path `/{foo}/{bar}`
///     // is required
///     .job("/{foo}/{bar}", delete(invalid_task))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
pub struct Target<T>(pub T);

impl<T: DeserializeOwned> FromContext for Target<T> {
    type Error = ExtractionError;

    fn from_context(context: &Context) -> Result<Self, Self::Error> {
        let value = &context.target;

        // This will fail if the value cannot be deserialized into the target type
        let target = serde_json::from_value::<T>(value.clone()).with_context(|| {
            format!(
                "Failed to deserialize {value} into {}",
                std::any::type_name::<T>()
            )
        })?;

        Ok(Target(target))
    }
}

impl<T: DeserializeOwned> FromSystem for Target<T> {
    type Error = ExtractionError;

    fn from_system(_: &System, context: &Context) -> Result<Self, Self::Error> {
        Self::from_context(context)
    }
}

impl<T> Deref for Target<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

//! Global error type definitions

use std::ops::Deref;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("argument extraction failed: {0:?}")]
/// Job argument extraction failed
///
/// This is likely to be an error with the task definition.
pub struct ExtractionError(#[from] anyhow::Error);

#[derive(Debug, Error)]
#[error("serialization error: {0:?}")]
/// An error happened while serializing or deserializing an input type
pub struct SerializationError(#[from] serde_json::Error);

#[derive(Debug, Error)]
#[error("internal error, this may be a bug: {0:?}")]
/// Some unexpected error happened during the worker operation
///
/// These errors should not happen, unless there is a bug in the implementation.
pub struct InternalError(#[from] anyhow::Error);

#[derive(Debug, Error)]
#[error("method expansion failed: {0:?}")]
/// An error happened while trying to expand the method into sub-tasks
///
/// This is likely an issue with the method definition or indicates a that a job on which the
/// method depends has not been registered with the Worker.
pub struct MethodError(Box<dyn std::error::Error + Send + Sync>);

impl MethodError {
    pub fn new<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Self(Box::new(err))
    }
}

#[derive(Debug, Error)]
#[error(transparent)]
/// An error happened while executing the task within the workflow.
///
/// These errors only happen at runtime, never at the planning stage
/// of the worker.
pub struct IOError(Box<dyn std::error::Error + Send + Sync>);

impl IOError {
    pub fn new<E: std::error::Error + Send + Sync + 'static>(err: E) -> Self {
        Self(Box::new(err))
    }
}

impl Deref for IOError {
    type Target = Box<dyn std::error::Error + Send + Sync>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::{self, Display};
use std::marker::PhantomData;
use std::ops::Deref;

use crate::error::{self, IntoError};
use crate::system::{FromSystem, System};
use crate::task::Context;

#[derive(Debug)]
pub enum TargetExtractError {
    Undefined,
    SerializationFailed(serde_json::error::Error),
    DeserializationFailed(serde_json::error::Error),
    CannotResolvePath {
        path: String,
        reason: jsonptr::resolve::ResolveError,
    },
}

impl std::error::Error for TargetExtractError {}

impl Display for TargetExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetExtractError::SerializationFailed(err) => err.fmt(f)?,
            TargetExtractError::DeserializationFailed(err) => err.fmt(f)?,
            TargetExtractError::Undefined => {
                write!(f, "no target has been defined for the context")?;
            }
            TargetExtractError::CannotResolvePath { path, reason } => {
                write!(f, "cannot resolve state path `{}`: {}", path, reason)?;
            }
        };
        Ok(())
    }
}

impl IntoError for TargetExtractError {
    fn into_error(self) -> error::Error {
        error::Error::TargetExtractFailed(self)
    }
}

pub struct Target<S: Clone, T = S> {
    target: T,
    _system: PhantomData<S>,
}

impl<S: Serialize + Clone, T: DeserializeOwned> FromSystem<S> for Target<S, T> {
    type Error = TargetExtractError;

    fn from_system(_: &System, context: &Context<S>) -> Result<Self, Self::Error> {
        if context.target.is_none() {
            return Err(TargetExtractError::Undefined);
        }

        let tgt = serde_json::to_value(context.target.clone())
            .map_err(TargetExtractError::SerializationFailed)?;

        // Return an error if the context path does not exist in the target object
        let pointer = context.path.as_ref();
        let value = pointer
            .resolve(&tgt)
            .map_err(|e| TargetExtractError::CannotResolvePath {
                path: context.path.to_string(),
                reason: e,
            })?;

        // This will fail if the value cannot be deserialized into the target type
        let target = serde_json::from_value::<T>(value.clone())
            .map_err(TargetExtractError::DeserializationFailed)?;

        Ok(Target {
            target,
            _system: PhantomData::<S>,
        })
    }
}

impl<S: Clone, T> Deref for Target<S, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.target
    }
}

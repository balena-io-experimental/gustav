use json_patch::{patch, Patch};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{
    fmt::{self, Display},
    ops::Deref,
};

use crate::error::{Error, IntoError};

mod from_system;
pub(crate) use from_system::*;

#[derive(Debug)]
pub enum SystemReadError {
    SerializationFailed(serde_json::error::Error),
}

impl std::error::Error for SystemReadError {}

impl Display for SystemReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SystemReadError::SerializationFailed(err) => err.fmt(f),
        }
    }
}

impl IntoError for SystemReadError {
    fn into_error(self) -> Error {
        Error::StateReadFailed(self)
    }
}

#[derive(Debug)]
pub struct SystemWriteError(json_patch::PatchError);

impl std::error::Error for SystemWriteError {}

impl Display for SystemWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl IntoError for SystemWriteError {
    fn into_error(self) -> Error {
        Error::StateWriteFailed(self)
    }
}

#[derive(Clone)]
pub struct System {
    state: Value,
}

impl<S> From<S> for System
where
    S: Serialize,
{
    fn from(state: S) -> Self {
        serde_json::to_value(state)
            .map(|state| Self { state })
            .expect("Input value to be serializable")
    }
}

impl System {
    pub(crate) fn root(&self) -> &Value {
        &self.state
    }

    pub(crate) fn root_mut(&mut self) -> &mut Value {
        &mut self.state
    }

    pub(crate) fn patch(&mut self, changes: Patch) -> Result<(), SystemWriteError> {
        patch(&mut self.state, &changes).map_err(SystemWriteError)?;
        Ok(())
    }

    pub fn state<S: DeserializeOwned>(&self) -> Result<S, SystemReadError> {
        let s = serde_json::from_value(self.state.clone())
            .map_err(SystemReadError::SerializationFailed)?;
        Ok(s)
    }
}

impl Deref for System {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

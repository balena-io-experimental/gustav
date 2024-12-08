use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("cannot read system state: ${0}")]
    StateReadFailed(#[from] super::system::SystemReadError),

    #[error("cannot write system state: ${0}")]
    StateWriteFailed(#[from] super::system::SystemWriteError),

    #[error("cannot extract path: ${0}")]
    PathExtractFailed(#[from] super::extract::PathDeserializationError),

    #[error("cannot extract target: ${0}")]
    TargetExtractFailed(#[from] super::extract::TargetExtractError),

    #[error("cannot extract view: ${0}")]
    ViewExtractFailed(#[from] super::extract::ViewExtractError),

    #[error("cannot calculate view result: ${0}")]
    ViewResultFailed(#[from] super::extract::ViewResultError),

    #[error("condition failed: ${0}")]
    TaskConditionFailed(#[from] super::task::ConditionFailed),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error>),
}

pub trait IntoError {
    fn into_error(self) -> Error;
}

impl IntoError for Error {
    fn into_error(self) -> Error {
        self
    }
}

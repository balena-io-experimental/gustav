use std::fmt::{Display, Formatter};

use crate::errors::ExtractionError;
use crate::system::System;
use crate::task::{Context, FromContext, FromSystem};

/// Extracts the full path that the Task is being applied to
///
/// # Example
///
/// ```rust,no_run
/// use mahler::{
///     extract::Path,
///     task::{Handler, update},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn foo_bar(Path(path): Path) {
///     // ...
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", update(foo_bar))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
pub struct Path(pub String);

impl FromContext for Path {
    type Error = ExtractionError;

    fn from_context(context: &Context) -> Result<Self, Self::Error> {
        let path = context.path.to_str();

        Ok(Path(String::from(path)))
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromSystem for Path {
    type Error = ExtractionError;

    fn from_system(_: &System, context: &Context) -> Result<Self, Self::Error> {
        Self::from_context(context)
    }
}

impl From<Path> for String {
    fn from(path: Path) -> Self {
        path.0
    }
}

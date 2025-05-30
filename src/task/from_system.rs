use crate::task::Error;

use super::System;
use crate::task::Context;

pub trait FromSystem: Sized {
    type Error: Into<Error> + 'static;

    fn from_system(state: &System, context: &Context) -> Result<Self, Self::Error>;

    /// The extractor is scoped if it only requires access to some
    /// part of the system state rather than to the global state.
    fn is_scoped() -> bool {
        true
    }
}

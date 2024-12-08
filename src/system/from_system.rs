use crate::error::IntoError;

use super::System;
use crate::task::Context;

pub trait FromSystem<S>: Sized {
    type Error: IntoError + 'static;

    fn from_system(state: &System, context: &Context<S>) -> Result<Self, Self::Error>;
}

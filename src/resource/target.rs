use super::Resource;
use crate::state::{FromState, State};
use std::ops::Deref;

pub struct Target<R>(R);

impl<R> FromState<R> for Target<R>
where
    R: Resource + Clone + 'static,
{
    fn from_state(_: &State, target: &R) -> Self {
        Self(target.clone())
    }
}

impl<R> Deref for Target<R> {
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
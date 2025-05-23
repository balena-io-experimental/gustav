use json_patch::Patch;

use super::Task;

use super::effect::Effect;
use super::errors::Error;
use crate::errors::{IOError, MethodError};

pub trait IntoResult<O> {
    fn into_result(self) -> Result<O, Error>;
}

/// Allow tasks to return an Option
impl<T, O> IntoResult<O> for Option<T>
where
    T: IntoResult<O>,
{
    fn into_result(self) -> Result<O, Error> {
        self.map(|value| value.into_result())
            .ok_or_else(|| Error::ConditionFailed)?
    }
}

impl From<()> for Effect<Patch, Error, ()> {
    fn from(value: ()) -> Effect<Patch, Error, ()> {
        Effect::of(value)
            .with_io(|value| async move { Ok(value) })
            .map(|_| Patch(vec![]))
    }
}

/// Allow tasks to return a value that implements
/// `IntoResult<Patch>`, e.g. View
impl<I> From<I> for Effect<Patch, Error, I>
where
    I: IntoResult<Patch> + Send + 'static,
{
    fn from(value: I) -> Effect<Patch, Error, I> {
        Effect::of(value).and_then(move |o| o.into_result())
    }
}

/// Implement `Into<Effect>` for any effect that has an `IntoResult` as the output. This means that, for
/// instance, `Effect<View<T>>` implements `Into<Effect>`, so effects can use extractors to interact
/// with the state
impl<I, E> From<Effect<I, E>> for Effect<Patch, Error, I>
where
    I: IntoResult<Patch> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(eff: Effect<I, E>) -> Effect<Patch, Error, I> {
        eff.map_err(|e| IOError::new(e).into())
            .and_then(move |o| o.into_result())
    }
}

// Allow tasks to return a pure `Vec<Task>`
// and this will convert them into an effect
impl From<Vec<Task>> for Effect<Vec<Task>, Error> {
    fn from(vec: Vec<Task>) -> Effect<Vec<Task>, Error> {
        Effect::of(vec)
    }
}

// Allow methods to return a task slice
// and this will convert them into an effect
impl<const N: usize> From<[Task; N]> for Effect<Vec<Task>, Error> {
    fn from(slice: [Task; N]) -> Effect<Vec<Task>, Error> {
        Effect::of(slice.into())
    }
}

// Allow methods to return a single task
impl From<Task> for Effect<Vec<Task>, Error> {
    fn from(task: Task) -> Effect<Vec<Task>, Error> {
        Effect::of(vec![task])
    }
}

impl<T> From<Option<T>> for Effect<Vec<Task>, Error>
where
    T: Into<Effect<Vec<Task>, Error>>,
{
    fn from(opt: Option<T>) -> Effect<Vec<Task>, Error> {
        opt.map(|t| t.into())
            .unwrap_or_else(|| Effect::from_error(Error::ConditionFailed))
    }
}

impl<T, E> From<Result<T, E>> for Effect<Vec<Task>, Error>
where
    T: Into<Effect<Vec<Task>, Error>>,
    E: std::error::Error + Send + Sync + 'static,
{
    fn from(res: Result<T, E>) -> Effect<Vec<Task>, Error> {
        res.map(|t| t.into())
            .unwrap_or_else(|e| Effect::from_error(MethodError::new(e).into()))
    }
}

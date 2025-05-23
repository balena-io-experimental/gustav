use std::{convert::Infallible, future::Future};

use super::effect::Effect;
use crate::extract::{Pointer, View};

/// Creates an [`Effect`] from a pure value and an I/O function
///
/// This function is useful for creating an effect that
/// combines a pure value with an I/O operation.
pub fn with_io<T, E, F, Res>(pure: T, io: F) -> Effect<T, E>
where
    T: Send + 'static,
    F: FnOnce(T) -> Res + Send + 'static,
    Res: Future<Output = Result<T, E>> + Send,
{
    Effect::of(pure).with_io(io)
}

/// Convenience alias for `Job` return type
pub type IO<T, E = Infallible> = Effect<View<T>, E>;

/// Convenience alias for `Job` return type
pub type Update<T, E = Infallible> = Effect<View<T>, E>;

/// Convenience alias for `Job` return type
pub type Delete<T, E = Infallible> = Effect<Pointer<T>, E>;

/// Convenience alias for `Job` return type
pub type Create<T, E = Infallible> = Effect<Pointer<T>, E>;

/// Convenience alias for `Job` return type
pub type Any<T, E = Infallible> = Effect<Pointer<T>, E>;

use json_patch::Patch;

use super::{Context, Effect, IntoEffect, IntoResult, Job, Task};
use crate::error::{Error, IntoError};
use crate::system::{FromSystem, System};

pub trait Handler<S, T, O, I = O>: Clone + Send + 'static {
    fn call(self, system: &System, context: Context<S>) -> Effect<O, Error, I>;

    fn into_job(self) -> Job<S>;

    fn into_task(self, context: Context<S>) -> Task<S>
    where
        S: Send + Sync + 'static,
        T: Send + 'static,
        I: 'static,
    {
        self.into_job().into_task(context)
    }
}

/// Implement IntoEffect for any effect that has an IntoResult as the output. This means that, for
/// instance, Effect<View<T>> implements into effect, so effects can use extractors to interact
/// with the state
impl<I: IntoResult<Patch> + 'static, E: std::error::Error + 'static> IntoEffect<Patch, Error, I>
    for Effect<I, E>
{
    fn into_effect(self, system: &System) -> Effect<Patch, Error, I> {
        let system = system.clone();
        self.map_err(|e| Error::Other(Box::new(e)))
            .and_then(move |o| o.into_result(&system))
    }
}

impl<S> IntoEffect<Vec<Task<S>>, Error> for Vec<Task<S>> {
    fn into_effect(self, _: &System) -> Effect<Vec<Task<S>>, Error> {
        Effect::of(self)
    }
}

impl<S: 'static, E: std::error::Error + 'static> IntoEffect<Vec<Task<S>>, Error>
    for Result<Vec<Task<S>>, E>
{
    fn into_effect(self, _: &System) -> Effect<Vec<Task<S>>, Error> {
        Effect::from(self.map_err(|e| Error::Other(Box::new(e))))
    }
}

impl<S, const N: usize> IntoEffect<Vec<Task<S>>, Error> for [Task<S>; N] {
    fn into_effect(self, _: &System) -> Effect<Vec<Task<S>>, Error> {
        Effect::of(self.into())
    }
}

impl<S: 'static, E: std::error::Error + 'static, const N: usize> IntoEffect<Vec<Task<S>>, Error>
    for Result<[Task<S>; N], E>
{
    fn into_effect(self, _: &System) -> Effect<Vec<Task<S>>, Error> {
        Effect::from(
            self.map_err(|e| Error::Other(Box::new(e)))
                .map(|a| a.into()),
        )
    }
}

macro_rules! impl_action_handler {
    (
        $first:ident, $($ty:ident),*
    ) => {
        #[allow(non_snake_case, unused)]
        impl<S, F, $($ty,)* Res, I> Handler<S, ($($ty,)*), Patch, I> for F
        where
            S: 'static,
            F: FnOnce($($ty,)*) -> Res + Clone + Send +'static,
            Res: IntoEffect<Patch, Error, I>,
            $($ty: FromSystem<S>,)*
            I: 'static
        {

            fn call(self, system: &System, context: Context<S>) -> Effect<Patch, Error, I>{
                $(
                    let $ty = match $ty::from_system(&system, &context) {
                        Ok(value) => value,
                        Err(failure) => return Effect::from_error(failure.into_error())
                    };
                )*

                let res = (self)($($ty,)*);

                // Update the system
                res.into_effect(&system)
            }

            fn into_job(self) -> Job<S> {
                Job::from_action(self)
            }
        }
    };
}

impl_action_handler!(T1,);
impl_action_handler!(T1, T2);
impl_action_handler!(T1, T2, T3);
impl_action_handler!(T1, T2, T3, T4);
impl_action_handler!(T1, T2, T3, T4, T5);
impl_action_handler!(T1, T2, T3, T4, T5, T6);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15);
impl_action_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16);

macro_rules! impl_method_handler {
    (
        $first:ident, $($ty:ident),*
    ) => {
        #[allow(non_snake_case, unused)]
        impl<S, F, $($ty,)* Res> Handler<S, ($($ty,)*), Vec<Task<S>>> for F
        where
            S: 'static,
            F: FnOnce($($ty,)*) -> Res + Clone + Send +'static,
            Res: IntoEffect<Vec<Task<S>>, Error>,
            $($ty: FromSystem<S>,)*
        {

            fn call(self, system: &System, context: Context<S>) -> Effect<Vec<Task<S>>, Error> {
                $(
                    let $ty = match $ty::from_system(&system, &context) {
                        Ok(value) => value,
                        Err(failure) => return Effect::from_error(failure.into_error())
                    };
                )*

                let res = (self)($($ty,)*);

                // Update the system
                res.into_effect(&system)
            }

            fn into_job(self) -> Job<S> {
                Job::from_method(self)
            }
        }
    };
}

impl_method_handler!(T1,);
impl_method_handler!(T1, T2);
impl_method_handler!(T1, T2, T3);
impl_method_handler!(T1, T2, T3, T4);
impl_method_handler!(T1, T2, T3, T4, T5);
impl_method_handler!(T1, T2, T3, T4, T5, T6);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15);
impl_method_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16);

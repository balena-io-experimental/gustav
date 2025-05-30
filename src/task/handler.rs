use json_patch::Patch;
use serde::Serialize;

use super::from_system::FromSystem;
use super::{Action, Context, Effect, Error, Method, Task};
use crate::system::System;

pub trait Handler<T, O, I = O>: Clone + Sync + Send + 'static {
    fn call(&self, system: &System, context: &Context) -> Effect<O, Error, I>;

    /// Get the unique identifier of the job handler
    fn id(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Return true if the handler can be parallelized
    ///
    /// This means the handler only scoped extractors and
    /// no global extractors, allowing it to be used in parallel
    /// with other handler with non-conflicting scope.
    fn is_scoped(&self) -> bool;

    /// Create a task from the handler
    fn into_task(self) -> Task;

    /// Create a task from the handler with a specific target
    ///
    /// This is a convenience method that is equivalent to calling
    /// `handler.into_task().with_target(target)`.
    ///
    /// Important: This function will panic if serialization of the target into JSON fails
    /// Use `Task::try_target` if you want to handle the error. This is done for convenience as
    /// serialization errors should be rare and this makes the code more concise.
    fn with_target<S: Serialize>(self, target: S) -> Task {
        self.into_task().with_target(target)
    }

    /// Create a task from the handler with a specific path argument
    ///
    /// This is a convenience method that is equivalent to calling
    /// `handler.into_task().with_path()`.
    fn with_arg(self, key: impl AsRef<str>, value: impl Into<String>) -> Task {
        self.into_task().with_arg(key, value)
    }
}

macro_rules! impl_action_handler {
    (
        $first:ident, $($ty:ident),*
    ) => {
        #[allow(non_snake_case, unused)]
        impl<F, $($ty,)* Res, I> Handler<($($ty,)*), Patch, I> for F
        where
            F: Fn($($ty,)*) -> Res + Clone + Send + Sync +'static,
            Res: Into<Effect<Patch, Error, I>> + Send,
            $($ty: FromSystem,)*
            I: Send + 'static
        {

            fn call(&self, system: &System, context: &Context) -> Effect<Patch, Error, I>{
                $(
                    let $ty = match $ty::from_system(system, context) {
                        Ok(value) => value,
                        Err(failure) => {
                            return Effect::from_error(failure.into())
                        }
                    };
                )*

                let res = (self)($($ty,)*);

                // Convert to effect
                res.into()
            }

            fn is_scoped(&self) -> bool {
                // The handler is scoped if all of its arguments are scoped
                true $(&& $ty::is_scoped())*
            }

            fn into_task(self) -> Task {
                Action::new(self, Context::default()).into()
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
        impl<F, $($ty,)* Res> Handler<($($ty,)*), Vec<Task>> for F
        where
            F: Fn($($ty,)*) -> Res + Clone + Send + Sync +'static,
            Res: Into<Effect<Vec<Task>, Error>>,
            $($ty: FromSystem,)*
        {

            fn call(&self, system: &System, context: &Context) -> Effect<Vec<Task>, Error> {
                $(
                    let $ty = match $ty::from_system(system, context) {
                        Ok(value) => value,
                        Err(failure) => {
                            return Effect::from_error(failure.into())
                        }
                    };
                )*

                let res = (self)($($ty,)*);

                // Convert to effect
                res.into()
            }

            fn is_scoped(&self) -> bool {
                // The handler is scoped if all of its arguments are scoped
                true $(&& $ty::is_scoped())*
            }

            fn into_task(self) -> Task {
                Method::new(self, Context::default()).into()
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

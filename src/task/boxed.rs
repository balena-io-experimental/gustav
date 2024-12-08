use json_patch::Patch;

use super::{Context, Handler, Task};

pub(crate) struct BoxedIntoTask<S>(Box<dyn ErasedIntoTask<S>>);

impl<S> BoxedIntoTask<S> {
    pub fn from_action<A, T, I>(action: A) -> Self
    where
        A: Handler<S, T, Patch, I>,
        S: 'static,
        I: 'static,
    {
        Self(Box::new(MakeIntoTask {
            handler: action,
            into_task: |id, handler, context| Task::atom(id, handler, context),
        }))
    }

    pub fn from_method<M, T>(method: M) -> Self
    where
        M: Handler<S, T, Vec<Task<S>>>,
        S: 'static,
    {
        Self(Box::new(MakeIntoTask {
            handler: method,
            into_task: |id, method, context| Task::list(id, method, context),
        }))
    }

    pub fn into_task(self, id: &str, context: Context<S>) -> Task<S> {
        self.0.into_task(id, context)
    }
}

impl<S> Clone for BoxedIntoTask<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

trait ErasedIntoTask<S> {
    fn clone_box(&self) -> Box<dyn ErasedIntoTask<S>>;

    fn into_task(self: Box<Self>, id: &str, context: Context<S>) -> Task<S>;
}

struct MakeIntoTask<H, S> {
    pub(crate) handler: H,
    pub(crate) into_task: fn(&str, H, Context<S>) -> Task<S>,
}

impl<S, H> ErasedIntoTask<S> for MakeIntoTask<H, S>
where
    S: 'static,
    H: Clone + 'static,
{
    fn clone_box(&self) -> Box<dyn ErasedIntoTask<S>> {
        Box::new(Self {
            handler: self.handler.clone(),
            into_task: self.into_task,
        })
    }

    fn into_task(self: Box<Self>, id: &str, context: Context<S>) -> Task<S> {
        (self.into_task)(id, self.handler, context)
    }
}

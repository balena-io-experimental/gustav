use json_patch::Patch;
use std::cmp::Ordering;

use super::boxed::*;
use super::context::Context;
use super::{Handler, Task};

/// The Job degree denotes its cardinality or its position in a search tree
///
/// - Atom jobs are the leafs in the search tree, they define the work to be
///   executed and cannot be expanded
/// - List jobs define work in terms of other tasks, they are expanded recursively
///   in order to get to a list of atoms
#[derive(Clone, PartialEq, PartialOrd, Eq, Ord)]
enum Degree {
    List,
    Atom,
}

/// Jobs are generic work definitions. They can be converted to tasks
/// by calling into_task with a specific context.
///
/// Jobs are re-usable
pub struct Job<S> {
    id: String,
    degree: Degree,
    builder: BoxedIntoTask<S>,
}

impl<S> PartialEq for Job<S> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<S> PartialOrd for Job<S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<S> Eq for Job<S> {}

impl<S> Ord for Job<S> {
    fn cmp(&self, other: &Self) -> Ordering {
        // We order jobs by degree. When searching for applicable
        // jobs, we want to give List jobs priority over atomic jobs
        // as these can be used to direct the search
        self.degree.cmp(&other.degree)
    }
}

impl<S> Job<S> {
    pub(crate) fn from_action<A, T, I>(action: A) -> Self
    where
        A: Handler<S, T, Patch, I>,
        S: 'static,
        I: 'static,
    {
        let id = String::from(std::any::type_name::<A>());

        Self {
            id,
            builder: BoxedIntoTask::from_action(action),
            degree: Degree::Atom,
        }
    }

    pub(crate) fn from_method<M, T>(method: M) -> Self
    where
        M: Handler<S, T, Vec<Task<S>>>,
        S: 'static,
    {
        let id = String::from(std::any::type_name::<M>());
        Self {
            id,
            degree: Degree::List,
            builder: BoxedIntoTask::from_method(method),
        }
    }

    pub fn id(&self) -> &String {
        &self.id
    }

    pub fn into_task(&self, context: Context<S>) -> Task<S> {
        self.builder.clone().into_task(self.id.as_str(), context)
    }
}

use crate::task::{Handler, Job};
use std::cmp::Ordering;

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone)]
pub enum Operation {
    Create,
    Update,
    Delete,
    Any,
    None,
}

pub struct Intent<S> {
    pub(crate) operation: Operation,
    pub(crate) job: Job<S>,
    priority: u8,
}

impl<S> Intent<S> {
    pub(crate) fn new(job: Job<S>) -> Self {
        Intent {
            operation: Operation::Update,
            job,
            priority: u8::MAX,
        }
    }

    /// Set intent priority.
    ///
    /// This defines search priority when looking for jobs
    /// the lower the value, the higher the priority
    pub fn with_priority(self, priority: u8) -> Self {
        let Intent { operation, job, .. } = self;
        Intent {
            operation,
            job,
            priority,
        }
    }

    fn with_operation(self, operation: Operation) -> Self {
        let Intent { priority, job, .. } = self;
        Intent {
            operation,
            job,
            priority,
        }
    }
}

macro_rules! define_intent {
    ($func_name:ident, $operation:expr) => {
        pub fn $func_name<S, H, T, O, I>(handler: H) -> Intent<S>
        where
            H: Handler<S, T, O, I>,
            S: 'static,
            I: 'static,
        {
            Intent::new(handler.into_job()).with_operation($operation)
        }
    };
}

define_intent!(create, Operation::Create);
define_intent!(update, Operation::Update);
define_intent!(delete, Operation::Delete);
define_intent!(any, Operation::Any);
define_intent!(none, Operation::None);

impl<S> PartialEq for Intent<S> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl<S> Eq for Intent<S> {}

impl<S> PartialOrd for Intent<S> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<S> Ord for Intent<S> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.job
            .cmp(&other.job)
            .then(self.operation.cmp(&other.operation))
            .then(self.priority.cmp(&other.priority))
    }
}

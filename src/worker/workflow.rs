use crate::dag::Dag;
use crate::task::Task;

pub(crate) struct Action {
    /**
     * Unique id for the action. This is calculated from the
     * task is and the current runtime state expected
     * by the planner. This is used for loop detection in the plan.
     */
    id: String,

    /**
     * The task to execute
     *
     * Only atomic tasks should be added to a worflow item
     */
    task: Task,
}

pub struct Workflow(pub(crate) Dag<Action>);

impl Default for Workflow {
    fn default() -> Self {
        Workflow(Dag::default())
    }
}

//! Automated planning and execution of task workflows

use anyhow::anyhow;
use async_trait::async_trait;
use json_patch::Patch;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;
use tokio::{select, sync::RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tracing::{debug, error, field, instrument, span, Instrument, Level, Span};

#[cfg(feature = "logging")]
mod logging;

#[cfg(debug_assertions)]
mod testing;

#[cfg(debug_assertions)]
pub use testing::*;

#[cfg(feature = "logging")]
#[cfg_attr(docsrs, doc(cfg(feature = "logging")))]
pub use logging::init as init_logging;

use crate::errors::{IOError, InternalError, SerializationError};
use crate::planner::{Domain, Error as PlannerError, Planner};
use crate::system::{Resources, System};
use crate::task::{Error as TaskError, Job};
use crate::workflow::{channel, AggregateError, Interrupt, Sender, WorkflowStatus};

pub mod prelude {
    //! Types and traits for setting up a Worker
    pub use super::SeekTarget;
}

/// A panic happened in the Worker runtime
#[derive(Debug, Error)]
#[error(transparent)]
pub struct Panicked(#[from] tokio::task::JoinError);

/// Unrecoverable error during the worker runtime
#[derive(Debug, Error)]
pub enum FatalError {
    #[error(transparent)]
    /// Failed to serialize or deserialize the Worker current/target state
    Serialization(#[from] SerializationError),

    #[error(transparent)]
    /// A panic happened in the Worker runtime
    ///
    /// This most likely means that there is an uncaught panic in a task
    Panic(#[from] Panicked),

    #[error(transparent)]
    /// An error happened with a task during planning
    ///
    /// This most likely means there is a bug in a task. This error will
    /// only be returned if `debug_assertions` are set. Otherwise
    /// task errors are ignored by the planner
    Planning(#[from] TaskError),

    #[error(transparent)]
    /// An internal error occured during the worker operation
    ///
    /// This is probably a bug with mahler and it should be reported
    Internal(#[from] InternalError),
}

#[derive(Debug)]
/// Exit status from [`Worker::seek_target`]
pub enum SeekStatus {
    /// The worker has reached the target state
    Success,
    /// No workflow was found for the given target
    NotFound,
    /// Worker interrupted by user request
    Interrupted,
    /// An error happened while executing the workflow.
    Aborted(Vec<IOError>),
}

impl PartialEq for SeekStatus {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (SeekStatus::Success, SeekStatus::Success)
                | (SeekStatus::NotFound, SeekStatus::NotFound)
                | (SeekStatus::Interrupted, SeekStatus::Interrupted)
        )
    }
}

#[async_trait]
/// Helper trait to chain worker calls
pub trait SeekTarget<O, I = O> {
    /// Look for a workflow for the given target and execute it
    async fn seek_target(self, tgt: I) -> Result<Worker<O, Ready, I>, FatalError>;
}

impl Eq for SeekStatus {}

#[derive(Default)]
/// Helper type to interrupt the Worker on Drop
struct AutoInterrupt(Interrupt);

impl Drop for AutoInterrupt {
    fn drop(&mut self) {
        // Set the interrupt flag to true when the worker is dropped
        self.0.set();
    }
}

#[derive(Debug, Clone)]
/// Helper type to indicate that a state change happened on the worker
struct UpdateEvent;

/// Helper trait to implement the Typestate pattern for Worker
pub trait WorkerState {}

/// Initial state of a Worker
///
/// While in the `Uninitialized` state, jobs and resources may be
/// assigned to the Worker
pub struct Uninitialized {
    domain: Domain,
    resources: Resources,
}

/// Initialized worker state
///
/// This is the state where the `Worker` moves to after receiving an initial state.
///
/// At this point the Worker is ready to start seeking a target state
pub struct Ready {
    planner: Planner,
    system: Arc<RwLock<System>>,
    updates: broadcast::Sender<UpdateEvent>,
    patches: Sender<Patch>,
    writer_closed: Arc<Notify>,
    interrupt: AutoInterrupt,
    status: SeekStatus,
}

/// Final state of a Worker
///
/// No further Worker operations can be performed after this state
pub struct Stopped {}

impl WorkerState for Uninitialized {}
impl WorkerState for Ready {}
impl WorkerState for Stopped {}

/// Core component for workflow generation and execution
///
/// Given a target to [`Worker::seek_target`], the `Worker` will look for a plan that takes the
/// system from the current state to the target, execute the plan (workflow) and re-plan if some
/// pre-condition failed at runtime.
///
/// # Worker setup
///
/// A worker may be in one of the following states
/// - `Uninitialized` is the initial state of the worker, while the worker is in this state, new
///   [jobs](`Worker::job`) and [resources](`Worker::resource`) may be configured to the worker.
/// - `Ready` this is the default state. The `Worker` goes into this state when an [initial
///   state](`Worker::initial_state`) been defined or when a `seek_target` operation terminates
///   without error.
/// - `Stopped` is the final state of the Worker. The worker will go into this state if [`Worker::stop`] is
///   called and no furher operations can be performed.
///
///
/// ```rust,no_run
/// use anyhow::{Context, Result};
/// use std::collections::HashMap;
/// use serde::{Deserialize, Serialize};
///
/// use mahler::worker::{Worker, SeekTarget, SeekStatus};
/// use mahler::task::prelude::*;
///
/// #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
/// struct Counters(HashMap<String, i32>);
///
/// // A simple job to update a counter
/// fn plus_one() -> Update<i32> { todo!() }
///
/// // A composite job
/// fn plus_two() -> Vec<Task> { todo!() }
///
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     // create a new uninitialized worker
///     let worker = Worker::new()
///         // configure jobs
///         .job("/{counter}", update(plus_one))
///         .job("/{counter}", update(plus_two))
///         // initialize the worker moving it to the `Ready` state
///         .initial_state(Counters(HashMap::from([
///             ("a".to_string(), 0),
///             ("b".to_string(), 0),
///         ])))
///         // start searching for a target
///         .seek_target(Counters(HashMap::from([
///             ("a".to_string(), 1),
///             ("b".to_string(), 2),
///         ])))
///         // wait for a result
///         .await
///         // fail if something bad happens
///         .with_context(|| "failed to reach target state")?;
///
///     if matches!(worker.status(), SeekStatus::Success) {
///         println!("SUCCESS!");
///     }
///
///     Ok(())
/// }
/// ```
///
/// # State type compatibility
///
/// Note that the Worker may use a different type for the internal state `<O>`
/// and the target state `<I>`. This is because the internal state may have additional
/// fields that may not be desirable to consider when comparing states (e.g runtime states
/// timestamps, variable data, etc.). These types must be compatible to ensure the Worker
/// can convert between them without issues.
///
/// In the example below, `InternalState` can be serialized into `TargetState` and
/// vice-versa.
///
/// ```rust
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// struct SystemState {
///     pub config: String,
///     pub last_update: Option<String>,
/// }
///
/// struct TargetState {
///     pub config: String,
/// }
/// ```
///
/// Serialization will fail between the two states below as `other_config` is not nullable.
///
/// ```rust
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Serialize, Deserialize)]
/// struct SystemState {
///     pub config: String,
///     pub other_config: String,
/// }
///
/// struct TargetState {
///     pub config: String,
/// }
/// ```
pub struct Worker<O, S: WorkerState = Uninitialized, I = O> {
    inner: S,
    _output: std::marker::PhantomData<O>,
    _input: std::marker::PhantomData<I>,
}

impl<O, S: WorkerState, I> Worker<O, S, I> {
    /// Create a worker from a inner [`WorkerState`]
    fn from_inner(inner: S) -> Self {
        Worker {
            inner,
            _output: std::marker::PhantomData,
            _input: std::marker::PhantomData,
        }
    }

    /// Stop following system updates
    ///
    /// This drops all internal structure for the Worker and no further
    /// operations can be performed after this is called.
    pub fn stop(self) -> Worker<O, Stopped, I> {
        Worker::from_inner(Stopped {})
    }
}

// Worker initialization

impl<O> Default for Worker<O, Uninitialized> {
    fn default() -> Self {
        Worker::new()
    }
}

impl<O> Worker<O, Uninitialized> {
    /// Create a new uninitialized Worker instance
    pub fn new() -> Self {
        Worker::from_inner(Uninitialized {
            domain: Domain::new(),
            resources: Resources::new(),
        })
    }
}

impl<O> Worker<O, Uninitialized> {
    /// Add a [Job](`crate::task::Job`) to the worker domain
    pub fn job(mut self, route: &'static str, job: Job) -> Self {
        self.inner.domain = self.inner.domain.job(route, job);
        self
    }

    /// Add a list if jobs linked to the same route on the worker domain
    ///
    /// This is a convenience method to simplify the configuration of multiple jobs
    /// for the same domain
    ///
    /// ```rust
    /// use serde::{Deserialize, Serialize};
    /// use mahler::worker::{Worker, Uninitialized};
    /// use mahler::task::prelude::*;
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct StateModel;
    ///
    /// fn foo() {}
    /// fn bar() {}
    ///
    /// let worker: Worker<StateModel, Uninitialized> = Worker::new()
    ///         .jobs("/{foo}", [update(foo), update(bar)]);
    /// ```
    pub fn jobs<const N: usize>(mut self, route: &'static str, list: [Job; N]) -> Self {
        self.inner.domain = self.inner.domain.jobs(route, list);
        self
    }

    /// Add a shared resource to use within tasks
    ///
    /// Resources are stored by [TypeId](`std::any::TypeId`),
    /// meaning only one resource of each type is allowed. If multiple resources of the same type
    /// are configured, only the last one will be used.
    ///
    /// ```rust
    /// use serde::{Deserialize, Serialize};
    /// use mahler::worker::{Worker, Uninitialized};
    ///
    /// #[derive(Serialize, Deserialize)]
    /// struct StateModel;
    ///
    /// // MyConnection represents a shared resource
    /// struct MyConnection;
    ///
    /// let conn = MyConnection {/* .. */};
    /// let otherconn = MyConnection { /* .. */};
    ///
    /// let worker: Worker<StateModel, Uninitialized> = Worker::new()
    ///         // add conn as resource
    ///         .resource(conn)
    ///         // only the last assignment of a resource of the same type
    ///         // will be used
    ///         .resource(otherconn);
    /// ```
    ///
    /// Resources can be accessed by jobs using the [Res extractor](`crate::extract::Res`).
    ///
    /// ```rust
    /// use mahler::task::prelude::*;
    /// use mahler::extract::Res;
    ///
    /// struct MyConnection;
    ///
    /// fn job_with_resource(res: Res<MyConnection>) {
    ///     // Access the worker MyConnection instance by dereferencing `res`.
    ///     // Initializing the extractor will fail if no resource of type MyConnection
    ///     // has been assigned to the worker.
    /// }
    /// ```
    pub fn resource<R>(mut self, res: R) -> Self
    where
        R: Send + Sync + 'static,
    {
        self.inner.resources = self.inner.resources.with_res(res);
        self
    }

    /// Provide the initial worker state
    ///
    /// This moves the state of the worker to `Ready`. No further jobs or resources may
    /// be assigned after `initial_state` has been called.
    ///
    /// Note that an additional input type `<I>` may be defined at this point in case
    /// a different model is needed for the target state. See [State Type Compatibility](#state-type-compatibility).
    ///
    /// # Errors
    /// The method will throw a [SerializationError](`crate::errors::SerializationError`) if the
    /// provided state cannot be converted to the internal state representation.
    pub fn initial_state<I>(self, state: O) -> Result<Worker<O, Ready, I>, SerializationError>
    where
        O: Serialize,
    {
        let Uninitialized {
            domain,
            resources: env,
            ..
        } = self.inner;

        let system = System::try_from(state).map(|s| s.with_resources(env))?;

        // Shared system protected by RwLock
        let system = Arc::new(RwLock::new(system));

        // Create the messaging channel
        let (tx, mut rx) = channel::<Patch>(100);

        // Patch error signal (notify)
        let notify = Arc::new(Notify::new());
        let writer_closed = notify.clone();

        // Broadcast channel for state updates
        let (updates, _) = broadcast::channel(1);

        // Spawn system writer task
        {
            let sys_writer = Arc::clone(&system);
            let broadcast = updates.clone();
            tokio::spawn(
                async move {
                    while let Some(mut msg) = rx.recv().await {
                        let changes = std::mem::take(&mut msg.data);
                        debug!("received changes: {:?}", changes);

                        let mut system = sys_writer.write().await;
                        if let Err(e) = system.patch(changes) {
                            // we need to abort on patch failure a this means the
                            // internal state may have become inconsistent and we cannot continue
                            // applying changes
                            error!("system patch failed: {e}");
                            notify.notify_one();
                            break;
                        }

                        // Notify the change over the broadcast channel
                        let _ = broadcast.send(UpdateEvent);

                        // yield back to the workflow
                        msg.ack();
                    }
                }
                .instrument(span!(Level::DEBUG, "system_writer")),
            );
        }

        Ok(Worker::from_inner(Ready {
            planner: Planner::new(domain),
            system,
            updates,
            patches: tx,
            writer_closed,
            interrupt: AutoInterrupt::default(),
            status: SeekStatus::Success,
        }))
    }
}

// -- Worker is ready to receive a target state

/// Helper type to receive Worker state changes
///
/// See [`Worker::follow`]
struct FollowStream<T> {
    inner: Pin<Box<dyn Stream<Item = T> + Send + 'static>>,
}

impl<T> FollowStream<T> {
    /// Create a new FollowStream from a Stream
    fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = T> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl<T> Stream for FollowStream<T> {
    type Item = T;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Returns a stream of updated states after each system change.
///
/// The stream is best effort, meaning updates may be missed if the receiver lags behind.
fn follow_worker<T>(
    updates: broadcast::Sender<UpdateEvent>,
    syslock: Arc<RwLock<System>>,
) -> impl Stream<Item = T>
where
    T: DeserializeOwned,
{
    let rx = updates.subscribe();
    FollowStream::new(
        BroadcastStream::new(rx)
            .then(move |res| {
                let sys_reader = Arc::clone(&syslock);
                async move {
                    if res.is_err() {
                        return None;
                    }
                    // Read the system state
                    let system = sys_reader.read().await;
                    system.state::<T>().ok()
                }
            })
            .filter_map(|opt| opt),
    )
}

impl<O, I> Worker<O, Ready, I> {
    /// Read the current system state from the worker
    ///
    /// The system state is behind a [RwLock](`tokio::sync::RwLock`)
    /// to allow for concurrent modification, which is why this method is `async`.
    ///
    /// # Errors
    ///
    /// The method will throw a [SerializationError](`crate::errors::SerializationError`) if the
    /// internal state cannot be deserialized into the output type `<O>`
    pub async fn state(&self) -> Result<O, SerializationError>
    where
        O: DeserializeOwned,
    {
        let system = self.inner.system.read().await;
        let state = system.state()?;
        Ok(state)
    }

    /// Read the current system state from the worker using the input type `<I>`
    ///
    /// This is a helper method to allow state manipulation
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use serde::{Deserialize, Serialize};
    /// use mahler::worker::{Worker, SeekTarget};
    ///
    /// #[derive(Debug, Serialize, Deserialize)]
    /// struct SystemState;
    ///
    /// #[derive(Debug, Serialize, Deserialize)]
    /// struct TargetState;
    ///
    /// # tokio_test::block_on(async move {
    /// let worker = Worker::new()
    ///     // todo: configure jobs
    ///     .initial_state(SystemState {/* .. */})
    ///     .seek_target(TargetState {/* .. */})
    ///     .await
    ///     .unwrap();
    ///
    /// let state = worker.state_as_target().await.unwrap();
    ///
    /// // todo: modify state
    /// let new_target = state;
    ///
    /// // trigger new search
    /// worker.seek_target(new_target);
    /// # })
    /// ```
    ///
    /// # Errors
    ///
    /// The method will throw a [SerializationError](`crate::errors::SerializationError`) if the
    /// internal state cannot be deserialized into the output type `<O>`
    pub async fn state_as_target(&self) -> Result<I, SerializationError>
    where
        I: DeserializeOwned,
    {
        let system = self.inner.system.read().await;
        let state = system.state()?;
        Ok(state)
    }

    /// Returns a stream of updated states after each system change
    ///
    /// The stream is best effort, meaning updates may be missed if the receiver lags behind.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use serde::{Deserialize, Serialize};
    /// use mahler::worker::{Worker, SeekTarget};
    /// use tokio_stream::StreamExt;
    ///
    /// #[derive(Debug, Serialize, Deserialize)]
    /// struct SystemState;
    ///
    /// # tokio_test::block_on(async move {
    /// let worker = Worker::new()
    ///     // todo: configure jobs
    ///     .initial_state(SystemState {/* .. */}).unwrap();
    ///
    /// // Get a state stream
    /// let mut update_stream = worker.follow();
    /// tokio::spawn(async move {
    ///     while let Some(state) = update_stream.next().await {
    ///         println!("Updated State: {:?}", state);
    ///     }
    /// });
    ///
    /// // Start state search
    /// let worker = worker.seek_target(SystemState {/* .. */})
    ///     .await
    ///     .unwrap();
    /// # })
    /// ```
    pub fn follow(&self) -> impl Stream<Item = O>
    where
        O: DeserializeOwned,
    {
        follow_worker(self.inner.updates.clone(), Arc::clone(&self.inner.system))
    }

    /// Return the result of the last worker run
    ///
    /// It will return `SeekStatus::Success` for a newly initialized `Worker`
    pub fn status(&self) -> &SeekStatus {
        &self.inner.status
    }

    #[instrument(skip_all, fields(return=field::Empty), err)]
    /// Trigger system changes by providing a new target state for the worker
    ///
    /// When called, this method tells the worker to look for a plan for the given
    /// target. If a plan is found, the worker then will try to execute the resulting workflow and
    /// and terminate if interrupted or a runtime error occurs. If a requirement changes between
    /// planning and runtime, the Worker triggers a re-plan.
    ///
    /// If no plan is found, the search terminates with a [`SeekStatus::NotFound`].
    ///
    /// # Errors
    /// The method will result in a [`FatalError`] if a serialization issue occurs while converting
    /// between state types, if the worker runtime panics or there is an unexpected error during
    /// planning.
    pub async fn seek_target(self, tgt: I) -> Result<Worker<O, Ready, I>, FatalError>
    where
        I: Serialize + DeserializeOwned,
    {
        let cur_span = Span::current();
        let tgt = serde_json::to_value(tgt).map_err(SerializationError::from)?;

        let Ready {
            planner,
            system,
            updates,
            writer_closed,
            patches,
            interrupt,
            ..
        } = self.inner;

        enum SeekResult {
            TargetReached,
            WorkflowCompleted,
            Interrupted,
        }

        enum SeekError {
            Runtime(AggregateError<TaskError>),
            Planning(PlannerError),
        }

        async fn find_and_run_workflow<I: Serialize + DeserializeOwned>(
            planner: &Planner,
            sys: &Arc<RwLock<System>>,
            tgt: &Value,
            channel: &Sender<Patch>,
            sigint: &Interrupt,
        ) -> Result<SeekResult, SeekError> {
            let workflow = {
                let system = sys.read().await;
                planner
                    .find_workflow::<I>(&system, tgt)
                    .map_err(SeekError::Planning)?
            };

            if workflow.is_empty() {
                return Ok(SeekResult::TargetReached);
            }

            let status = workflow
                .execute(sys, channel.clone(), sigint.clone())
                .await
                .map_err(SeekError::Runtime)?;

            if matches!(status, WorkflowStatus::Interrupted) {
                return Ok(SeekResult::Interrupted);
            }

            Ok(SeekResult::WorkflowCompleted)
        }

        let err_rx = writer_closed.clone();

        // Main seek_target planning and execution loop
        let handle: JoinHandle<Result<(Planner, SeekStatus), FatalError>> = {
            let task_int = interrupt.0.clone();
            let workflow_int = task_int.clone();
            let sys_reader = Arc::clone(&system);
            let changes = patches.clone();
            tokio::spawn(async move {
                loop {
                    select! {
                        biased;

                        _ = err_rx.notified() => {
                            return Err(InternalError::from(anyhow!("state patch failed, worker state possibly tainted")))?;
                        }

                        _ = workflow_int.wait() => {
                            cur_span.record("return", "interrupted");
                            return Ok((planner, SeekStatus::Interrupted));
                        }

                        res = find_and_run_workflow::<I>(&planner, &sys_reader, &tgt, &changes, &task_int) => {
                            match res {
                                Ok(SeekResult::TargetReached) => {
                                    cur_span.record("return", "success");
                                    return Ok((planner, SeekStatus::Success));
                                }
                                Ok(SeekResult::WorkflowCompleted) => {}
                                Ok(SeekResult::Interrupted) => {
                                    cur_span.record("return", "interrupted");
                                    return Ok((planner, SeekStatus::Interrupted));
                                }
                                Err(SeekError::Planning(PlannerError::NotFound)) =>  return Ok((planner, SeekStatus::NotFound)),
                                Err(SeekError::Planning(PlannerError::Serialization(e))) =>  return Err(e)?,
                                Err(SeekError::Planning(PlannerError::Internal(e))) =>  return Err(e)?,
                                Err(SeekError::Planning(PlannerError::Task(e))) => return Err(e)?,
                                Err(SeekError::Runtime(err)) => {
                                    let mut io = Vec::new();
                                    let mut other = Vec::new();
                                    let AggregateError (all) = err;
                                    for e in all.into_iter() {
                                        match e {
                                            TaskError::IO(re) => io.push(re),
                                            TaskError::ConditionFailed => {},
                                            _ => other.push(e)
                                        }

                                    }

                                    // If there are non-IO errors, there is
                                    // probably a bug somewhere
                                    if !other.is_empty() {
                                        return Err(InternalError::from(anyhow!(AggregateError::from(other))))?;
                                    }

                                    // Abort if there are any runtime errors as those
                                    // should be recoverable
                                    if !io.is_empty() {
                                        cur_span.record("return", "aborted");
                                        return Ok((planner, SeekStatus::Aborted(io)));
                                    }

                                    // If we got here, all errors were of type ConditionNotMet
                                    // in which case we re-plan as the state may have changed
                                    // underneath the worker
                                    continue;
                                }
                            }
                        }
                    }
                }
            })
        };

        let (planner, status) = match handle.await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(Panicked(e))?,
        }?;

        Ok(Worker::from_inner(Ready {
            planner,
            system,
            updates,
            patches,
            writer_closed,
            interrupt: AutoInterrupt::default(),
            status,
        }))
    }
}

#[async_trait]
impl<O: Send, I: Serialize + DeserializeOwned + Send> SeekTarget<O, I>
    for Result<Worker<O, Ready, I>, SerializationError>
{
    async fn seek_target(self, tgt: I) -> Result<Worker<O, Ready, I>, FatalError> {
        let worker = self?;
        worker.seek_target(tgt).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::extract::{Target, View};
    use crate::task::*;
    use serde::Deserialize;
    use tokio::time::{sleep, timeout};
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::{prelude::*, EnvFilter};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Counters(HashMap<String, i32>);

    fn plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> Effect<View<i32>> {
        if *counter < tgt {
            // Modify the counter if we are below target
            *counter += 1;
        }

        // Return the updated counter. The I/O part of the
        // effect will only be called if the job is chosen
        // in the workflow which will only happens if there are
        // changes
        Effect::of(counter).with_io(|counter| async {
            sleep(Duration::from_millis(10)).await;
            Ok(counter)
        })
    }

    fn buggy_plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> View<i32> {
        if *counter < tgt {
            // This is the wrong operation
            *counter -= 1;
        }

        counter
    }

    fn init() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_target(false)
                    .with_thread_names(true)
                    .with_thread_ids(true)
                    .with_line_number(true)
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
            )
            .with(EnvFilter::from_default_env())
            .try_init()
            .unwrap_or(());
    }

    #[tokio::test]
    async fn test_worker_complex_state() {
        init();
        let worker = Worker::new()
            .job("/{counter}", update(plus_one))
            .initial_state(Counters(HashMap::from([
                ("one".to_string(), 0),
                ("two".to_string(), 0),
            ])))
            .seek_target(Counters(HashMap::from([
                ("one".to_string(), 2),
                ("two".to_string(), 0),
            ])))
            .await
            .unwrap();

        assert_eq!(worker.status(), &SeekStatus::Success);
        let state = worker.state().await.unwrap();
        assert_eq!(
            state,
            Counters(HashMap::from([
                ("one".to_string(), 2),
                ("two".to_string(), 0),
            ]))
        );
    }

    #[tokio::test]
    async fn test_worker_bug() {
        init();
        let res = Worker::new()
            .job("", update(buggy_plus_one))
            .initial_state(0)
            .seek_target(2)
            .await
            .unwrap();

        assert!(matches!(res.status(), &SeekStatus::NotFound))
    }

    #[tokio::test]
    async fn test_worker_follow_updates() {
        init();
        let worker = Worker::new()
            .job("", update(plus_one))
            .initial_state(0)
            .unwrap();

        // Collect all results
        let mut updates = worker.follow();
        let results = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        {
            let results = Arc::clone(&results);
            tokio::spawn(async move {
                let mut res = results.write().await;
                // Capture two updates
                let first_update = updates.next().await;
                res.push(first_update);

                let second_update = updates.next().await;
                res.push(second_update);
            });
        }

        // Wait for worker to finish
        let worker = worker.seek_target(2).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        let results = results.read().await;
        assert_eq!(*results, vec![Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn test_worker_follow_best_effort_loss() {
        init();
        let worker = Worker::new()
            .job("", update(plus_one))
            .initial_state(0)
            .unwrap();

        let mut updates = worker.follow();
        let results = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        {
            let results = Arc::clone(&results);
            tokio::spawn(async move {
                let mut res = results.write().await;

                // Consume only some of the updates to simulate slow reader
                let first = updates.next().await;
                res.push(first);

                // Sleep to let many updates be missed
                tokio::time::sleep(Duration::from_millis(200)).await;

                // Attempt to read again (might be after some lag)
                let maybe_update = updates.next().await;
                res.push(maybe_update);
            });
        }

        // Wait for worker to finish
        let worker = worker.seek_target(100).await.unwrap();
        assert_eq!(worker.status(), &SeekStatus::Success);

        let results = results.read().await;
        assert_eq!(
            (*results)
                .iter()
                .map(|r| r.is_some())
                .collect::<Vec<bool>>(),
            vec![true, true]
        )
    }

    #[tokio::test]
    async fn test_worker_interrupt_status() {
        init();

        fn sleepy_plus_one(mut counter: View<i32>, Target(tgt): Target<i32>) -> Effect<View<i32>> {
            if *counter < tgt {
                // Modify the counter if we are below target
                *counter += 1;
            }

            // Return the updated counter. The I/O part of the
            // effect will only be called if the job is chosen
            // in the workflow which will only happens if there are
            // changes
            Effect::of(counter).with_io(|counter| async {
                sleep(Duration::from_millis(10)).await;
                Ok(counter)
            })
        }

        let worker = Worker::new()
            .job("", update(sleepy_plus_one))
            .initial_state(0)
            .unwrap();

        let mut updates = worker.follow();
        let results = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        {
            let results = Arc::clone(&results);
            tokio::spawn(async move {
                while let Some(s) = updates.next().await {
                    let mut res = results.write().await;
                    res.push(s);
                }
            });
        }

        // Ensure a timeout happens before the end of the run
        let res = timeout(Duration::from_millis(30), worker.seek_target(10)).await;
        assert!(res.is_err());

        // dropping the worker terminates the stream early
        let results = results.read().await;
        assert!(results.len() < 3);
    }

    #[tokio::test]
    async fn test_follow_stream_closes_on_worker_end() {
        init();
        let worker = Worker::new()
            .job("", update(plus_one))
            .initial_state(0)
            .unwrap();

        let mut updates = worker.follow();
        let results = Arc::new(tokio::sync::RwLock::new(Vec::new()));
        {
            let results = Arc::clone(&results);
            tokio::spawn(async move {
                let mut res = results.write().await;

                // After worker finishes, stream should terminate
                let first = updates.next().await;
                res.push(first);

                let end = updates.next().await;
                res.push(end);
            });
        }

        let worker = worker.seek_target(1).await.unwrap();

        // Wait for worker to finish
        assert_eq!(worker.status(), &SeekStatus::Success);

        // Close the stream
        worker.stop();

        let results = results.read().await;
        assert_eq!(*results, vec![Some(1), None]);
    }
}

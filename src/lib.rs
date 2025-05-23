//! mahler is an automated job orchestration library to build and execute dynamic workflows.
//!
//! The library uses [automated planning](https://en.wikipedia.org/wiki/Automated_planning_and_scheduling) (heavily based on [HTNs](https://en.wikipedia.org/wiki/Hierarchical_task_network)) to compose user defined jobs into a workflow (represented as a [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph)) to achieve a desired target system state.
//!
//! # Features
//!
//! - Simple API. Jobs can be targeted to specific paths and operations within the state model for targeted operations.
//! - Declaratively access System state and resources using extractors.
//! - Intelligent planner. Automatically discover a workflow to transition from the current system state to a given target state.
//! - Concurrent execution of jobs. The planner automatically detects when operations can be performed in parallel and adjusts the execution graph for concurrency.
//! - Observable runtime. Monitor the evolving state of the system from the Worker API. For more detailed logging, the library uses the [tracing crate](https://github.com/tokio-rs/tracing/).
//! - Easy to debug. Agent observable state and known goals allow easy replicability when issues occur.
//!
//! # Worker
//!
//! A [Worker](`worker::Worker`) orchestrates jobs into a workflow and executes the workflow tasks
//! when given a target. The worker also manages the system state model, making changes as the
//! workflow is executed.
//!
//! When creating a Worker, the first step is to assign jobs to specific routes and operations within the system
//! state
//!
//! ```rust
//! use mahler::worker::Worker;
//! use mahler::task::prelude::*;
//!
//! let worker = Worker::new()
//!         .job("", update(global))
//!         .job("/{foo}", update(foo))
//!         .job("/{foo}/{bar}", create(foo_bar));
//!
//! fn global() {}
//! fn foo() {}
//! fn foo_bar() {}
//! ```
//!
//! Providing the worker with an initial state makes the worker ready to operate on the system
//!
//! ```rust
//! let worker = worker.initial_state(StateModel::default())?;
//! ```
//!
//! The only way to control the worker and effect changes into the controlled system is to provide
//! the worker with a target state.
//!
//! ```rust
//! let worker = worker.seek_target(StateModel { .. }).await?;
//! ```
//!
//! When comparing the internal state with the target, the Worker's planner will generate a list of
//! differences between the states (see [JSON Patch](https://datatracker.ietf.org/doc/html/rfc6902)) and try
//! to find a plan within the jobs that are applicable to a path.
//!
//! For instance, in the worker defined above, if the target finds a new value is created in the path `/a/b`
//! it will try jobs from more general to more specific, meaning it will try
//!
//! - global
//! - foo
//! - foo_bar
//!
//! ## Operations
//!
//! Jobs may be applicable to operations `create` (add), `update` (replace), `delete` (remove), `any` and `none`,
//! meaning they may be selected when a new property is created/updated or removed from the system
//! state. A job assigned to `none` is never selected by the planner, but may be used as part of
//! [compound jobs](#compound-jobs).
//!
//! See [Operation](`task::Operation`) for more information.
//!
//! ## State
//!
//! The library relies on [JSON values](https://docs.rs/serde_json/latest/serde_json/enum.Value.html) for internal state
//! representation. Parts of the state can be referenced using [JSON pointers](https://docs.rs/jsonptr/latest/jsonptr/)
//! (see [RFC 6901](https://datatracker.ietf.org/doc/html/rfc6901)) and state differences are calculated using
//! [JSON patch](https://crates.io/crates/json-patch) (see [RFC 6902](https://datatracker.ietf.org/doc/html/rfc6902)).
//!
//! For this reason, the system state can only be modelled using [serializable data structures](https://serde.rs). Moreover,
//! non-serializable fields (annotated with [skip_serializing](https://serde.rs/attr-skip-serializing.html) will be
//! lost when passing data to the jobs.
//!
//! ```rust
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct MySystemState {
//!     // can be accessed from jobs
//!     some_value: String,
//!   
//!     // this will never be passed to jobs
//!     #[serde(skip_serializing)]
//!     other_value: String,
//! }
//! ```
//!
//! Mahler provides another mechanism for accessing read-only, non-serializable resources from
//! jobs.
//!
//! ## Shared resources
//!
//! Sometimes it may be desirable for jobs to access a shared resource (e.g. database connection,
//! file descriptor, etc.). These can be provided when creating the worker, with the only
//! restriction is that these structures must be `Send` and `Sync`.
//!
//! ```rust
//! let conn = initialize_connection();
//!
//! let worker = Worker::new()
//!         .resource::<MyConnection>(conn)
//! ```
//!
//! # Jobs and Tasks
//!
//! A [Job](`task::Job`) in Mahler is a repeatable operation defined as a Rust [Handler](`task::Handler`) that operates on the
//! system state and may or may not perform IO operations. A Job describes a generic operation and
//! can be converted to a concrete [Task](`task::Task`) by assigning a context. The context is composed of
//! an application path (a [JSON pointer](https://www.rfc-editor.org/rfc/rfc6901) to a part of the
//! system state), an optional target and zero or more path arguments.
//!
//!
//! ## Extractors
//!
//! ## Effect
//!
//! ## Compound Jobs
//!
//! # Error handling
//!
//! - Serialization errors
//! - Extraction error
//! - Expansion error
//! - IO (or runtime) errors
//!
//! # Observability
//! # Testing

mod path;
mod planner;
mod system;

pub mod errors;
pub mod extract;
pub mod task;
pub mod worker;
pub mod workflow;

// TODO: this should not be exported from this crate.
// It would more sense to re-export it, including the seq
// and dag macros, from a "mahler-test" crate
pub use workflow::Dag;

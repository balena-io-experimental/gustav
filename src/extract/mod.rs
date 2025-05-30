//! Types and traits for extracting task runtime context
//!
//! # Intro
//!
//! A [handler](`crate::task::Handler`) is any function that accepts zero or more "extractors" as
//! arguments and returns something that can be converted into an [Effect](`crate::task::Effect`) on the
//! system. An extractor is a type that implements [FromSystem](`crate::task::FromSystem`).
//!
//! ```rust,no_run
//! use mahler::extract::{View, Target};
//! use mahler::task::{Handler, update};
//! use mahler::worker::{Worker, Ready};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Counters {/* .. */}
//!
//! fn plus_one(mut counter: View<u32>, Target(tgt): Target<u32>) {
//!     // ...
//! }
//!
//! let worker: Worker<Counters, Ready> = Worker::new()
//!     .job("/counters/{counter}", update(plus_one))
//!     .initial_state(Counters {/* .. */})
//!     .unwrap();
//! ```
//!
//! # Common extractors
//!
//! Some commonly used extractors are
//!
//! ```rust
//! use mahler::extract::{View, Pointer, Args, Target, System, Res};
//!
//! struct MyConnection;
//! struct MySystemState;
//!
//! // `View` gives you a view into the relevant part of the
//! // state for the handler.
//! fn view(state: View<u32>) {}
//!
//! // `Pointer` is like `View`, except the pointed value can be null
//! // for instance, in the case of `create` operations
//! fn pointer(state: Pointer<u32>) {}
//!
//! // `Args` gives you the path arguments and deserializes them
//! fn args(Args(counter_name): Args<String>) {}
//!
//! // `Target` gives you the target value for the Job operation
//! // note that `delete` operations do not have a target.
//! fn target(Target(tgt): Target<u32>) {}
//!
//! // `System` provides a view into the top level system state.
//! // A Job using the System extractor is not parallelizable
//! fn system(System(state): System<MySystemState>) {}
//!
//! // `Res` allows to access a shared resource
//! fn res(res: Res<MyConnection>) {}
//! ```
//!
//! # Extractor scoping
//!
//! Job is assigned to a specific state path and its handler only allowed to read/write data
//! under that path. This is called *scoping* and it allows to ensure that instanced tasks are
//! parallelizable. However, sometimes a handler may require access to a property outside its path
//! order to make changes, this means the Job is no longer scoped.
//!
//! A handler is scoped if all its extractors are scoped.
//!
//! For now, the only non-scoped extractor is [`System`], which provides read-only access to the global
//! [Worker](`crate::worker::Worker`) state.
//!
//! # Human readable Job descriptions
//!
//! Some extractors implement [FromContext](`crate::task::FromContext`), this means that these
//! extractors can be initialized without need to access to the system and can thus be used as part
//! of [Job description](`crate::task::Description`).
//!
//! ```rust,no_run
//! use mahler::worker::{Worker, Uninitialized};
//! use mahler::task::prelude::*;
//! use mahler::extract::Args;
//!
//! struct SystemState;
//!
//! fn foo() {}
//!
//! let worker: Worker<SystemState, Uninitialized> = Worker::new()
//!     .job("/{foo}", update(foo)
//!             .with_description(|Args(foo): Args<String>| format!("this is {foo}"))
//!         );
//! ```

mod args;
mod path;
mod res;
mod system;
mod target;
mod view;

pub use args::*;
pub use path::*;
pub use res::*;
pub use system::*;
pub use target::*;
pub use view::*;

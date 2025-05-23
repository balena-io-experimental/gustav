use anyhow::Context as AnyhowCxt;
use std::ops::Deref;
use std::sync::Arc;

use crate::errors::ExtractionError;
use crate::system::System;
use crate::task::{Context, FromSystem};

/// Extracts a shared system resource
///
/// This extractor will look for a resource of type `<R>` for the [Worker](`crate::worker::Worker`) and
/// provides read-only access to a resource reference if it exists.
///
/// Internally, this extractor stores the resource as an `Arc<R>`;
///
/// # Example
///
/// Multiple `Res` extractors can be used in the same handler, but since `Worker` indexes resources
/// by `TypeId`, extracting the same resource twice will result in two references of the same
/// resource.
///
/// ```rust,no_run
/// use mahler::{
///     extract::Res,
///     task::{Handler, update},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// // A shared resource type
/// struct MyConnection {/* ... */};
///
/// // Another resource
/// struct MyConfig {/* ... */};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn multiple_resources(conn: Res<MyConnection>, config: Res<MyConfig>) {
///     // a reference to the resources configured in the Worker below
///     // can be access within the Job handler
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", update(multiple_resources))
///     .resource(MyConnection {/* ... */})
///     .resource(MyConfig {/* ... */})
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// There is nothing that prevents you from making a resource editable behind a `RwLock` (for
/// instance), however, this may interfere with workflow execution.
///
/// ```rust,no_run
/// use tokio::sync::RwLock;
/// use std::ops::Deref;
/// use mahler::{
///     extract::{View, Res},
///     task::{Handler, update, Update, with_io},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// // An editable resource
/// struct MyConfig(RwLock<String>);
/// impl MyConfig {
///     pub fn new(s: impl Into<String>) -> Self {
///         MyConfig(RwLock::new(s.into()))
///     }
/// }
///
/// impl Deref for MyConfig {
///     type Target = RwLock<String>;
///
///     fn deref(&self) -> &Self::Target {
///         &self.0
///     }
/// }
///
/// #[derive(Serialize, Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn edit_resources(view: View<i32>, config: Res<MyConfig>) -> Update<i32> {
///     // update view
///     with_io(view, |view| async move {
///         {
///             // this is possible but it may interfere with the workflow execution
///             // if there are multiple writers in parallel
///             let mut conf = config.write().await;
///             *conf = String::from("bar");
///         }
///
///         Ok(view)
///     })
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", update(edit_resources))
///     .resource(MyConfig::new("foo"))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// # Errors
/// Initialization of the extractor will fail if there is no resource of type `<R>` configured for
/// the `Worker`.
#[derive(Debug, Clone)]
pub struct Res<R>(Arc<R>);

impl<R: Send + Sync + 'static> FromSystem for Res<R> {
    type Error = ExtractionError;

    fn from_system(system: &System, _: &Context) -> Result<Self, Self::Error> {
        let arc = system
            .get_res::<R>()
            .with_context(|| {
                format!(
                    "failed to find resource of type {}",
                    std::any::type_name::<R>()
                )
            })
            .map_err(ExtractionError::from)?;
        Ok(Res(arc))
    }
}

impl<R> Deref for Res<R> {
    type Target = R;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

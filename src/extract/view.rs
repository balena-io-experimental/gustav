use anyhow::{anyhow, Context as AnyhowCtx};
use json_patch::{
    diff, AddOperation, CopyOperation, MoveOperation, Patch, PatchOperation, RemoveOperation,
    ReplaceOperation, TestOperation,
};
use jsonptr::resolve::ResolveError;
use jsonptr::PointerBuf;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::ops::{Deref, DerefMut};

use crate::errors::ExtractionError;
use crate::path::Path;
use crate::system::System;
use crate::task::{Context, Effect, Error, FromSystem, IntoResult};

/// Extracts a pointer to a sub-element of the global state indicated
/// by the path.
///
/// The type of the sub-element is given by the type parameter T.
///
/// The pointer can be null, meaning the parent of the element exists,
/// but the specific location pointed by the path does not exist.
///
/// # Example
///
/// ```rust,no_run
/// use mahler::{
///     extract::Pointer,
///     task::{Handler, create, with_io, Create},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn foo_bar(mut ptr: Pointer<i32>) -> Create<i32> {
///     // ptr will be "null" if the value does
///     // not exist beforehand
///     if ptr.is_none() {
///         ptr.zero();
///     }
///
///     with_io(ptr, |ptr| async {
///         // do something with ptr at runtime
///         Ok(ptr)
///     })
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", create(foo_bar))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// # Errors
///
/// Initializing the extractor will fail if the path assigned to the job cannot be resolved or the
/// value pointed by the path cannot be deserialized into type `<T>`
#[derive(Debug)]
pub struct Pointer<T> {
    initial: Value,
    state: Option<T>,
    path: Path,
}

impl<T> Pointer<T> {
    // The only way to create a pointer is via the
    // from_system method
    fn new(initial: Value, state: Option<T>, path: Path) -> Self {
        Pointer {
            initial,
            state,
            path,
        }
    }

    /// Assign a value to location indicated by the path.
    pub fn assign(&mut self, value: impl Into<T>) -> &mut T {
        self.insert(value.into())
    }

    /// Clear the value at the location indicated by the path
    pub fn unassign(mut self) -> Self {
        self.state.take();
        self
    }

    /// Initialize the location pointed by the path with the defaut value of the type T.
    pub fn zero(&mut self) -> &mut T
    where
        T: Default,
    {
        self.assign(T::default())
    }
}

impl<T: DeserializeOwned> FromSystem for Pointer<T> {
    type Error = ExtractionError;

    fn from_system(system: &System, context: &Context) -> Result<Self, Self::Error> {
        let json_ptr = context.path.as_ref();
        let root = system.root();

        // Use the parent of the pointer unless we are at the root
        let parent = json_ptr.parent().unwrap_or(json_ptr);

        // Try to resolve the parent or fail
        // XXX: how can this happen?
        parent
            .resolve(root)
            .with_context(|| format!("Failed to resolve path {}", context.path))?;

        // At this point we assume that if the pointer cannot be
        // resolved is because the value does not exist yet unless
        // the parent is a scalar
        let (state, initial): (Option<T>, Value) = match json_ptr.resolve(root) {
            Ok(value) => (
                Some(serde_json::from_value::<T>(value.clone()).with_context(|| {
                    format!(
                        "Failed to deserialize {value} into {}",
                        std::any::type_name::<T>()
                    )
                })?),
                value.clone(),
            ),
            Err(e) => match e {
                ResolveError::NotFound { .. } => (None, Value::Null),
                ResolveError::OutOfBounds { .. } => (None, Value::Null),
                _ => {
                    // XXX: how can this happen?
                    return Err(
                        anyhow!(e).context(format!("Failed to resolve path {}", context.path))
                    )?;
                }
            },
        };

        Ok(Pointer::new(initial, state, context.path.clone()))
    }
}

impl<T> Deref for Pointer<T> {
    type Target = Option<T>;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl<T> DerefMut for Pointer<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

fn prepend_path(pointer: PointerBuf, patch: Patch) -> Patch {
    let Patch(changes) = patch;
    let changes = changes
        .into_iter()
        .map(|op| match op {
            PatchOperation::Replace(ReplaceOperation { path, value }) => {
                PatchOperation::Replace(ReplaceOperation {
                    path: pointer.concat(&path),
                    value,
                })
            }
            PatchOperation::Remove(RemoveOperation { path }) => {
                PatchOperation::Remove(RemoveOperation {
                    path: pointer.concat(&path),
                })
            }
            PatchOperation::Add(AddOperation { path, value }) => {
                PatchOperation::Add(AddOperation {
                    path: pointer.concat(&path),
                    value,
                })
            }
            PatchOperation::Move(MoveOperation { from, path }) => {
                PatchOperation::Move(MoveOperation {
                    from,
                    path: pointer.concat(&path),
                })
            }
            PatchOperation::Copy(CopyOperation { from, path }) => {
                PatchOperation::Copy(CopyOperation {
                    from,
                    path: pointer.concat(&path),
                })
            }
            PatchOperation::Test(TestOperation { path, value }) => {
                PatchOperation::Test(TestOperation {
                    path: pointer.concat(&path),
                    value,
                })
            }
        })
        .collect::<Vec<PatchOperation>>();
    Patch(changes)
}

impl<T: Serialize> IntoResult<Patch> for Pointer<T> {
    fn into_result(self) -> Result<Patch, Error> {
        let before = self.initial;
        let after = if let Some(state) = self.state {
            // This should not happen unless there is a bug (hopefully).
            // if this happens during worker operation, it will be catched
            // as a panic in the task
            serde_json::to_value(state).expect("failed to serialize pointer value")
        } else {
            Value::Null
        };

        let patch = match (before, after) {
            (Value::Null, Value::Null) => Patch(vec![]),
            (Value::Null, after) => Patch(vec![PatchOperation::Add(AddOperation {
                path: self.path.into(),
                value: after,
            })]),
            (_, Value::Null) => Patch(vec![PatchOperation::Remove(RemoveOperation {
                path: self.path.into(),
            })]),
            (before, after) => prepend_path(self.path.into(), diff(&before, &after)),
        };

        Ok(patch)
    }
}

/// Convert a simple pointer into an effect
impl<T, E> From<Pointer<T>> for Effect<Pointer<T>, E> {
    fn from(ptr: Pointer<T>) -> Effect<Pointer<T>, E> {
        Effect::from_result(Ok(ptr))
    }
}

/// Extracts a pointer to a sub-element of the global state indicated
/// by the path.
///
/// The type of the sub-element is given by the type parameter T.
///
/// Differently from `Pointer`, the `View` extractor expects that the location
/// pointed by the Job path exists and extraction will fail during initialization otherwise.
///
/// # Example
///
/// ```rust,no_run
/// use mahler::{
///     extract::View,
///     task::{Handler, update, with_io, Update},
///     worker::{Worker, Ready}
/// };
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Serialize,Deserialize)]
/// struct SystemState {/* ... */};
///
/// fn foo_bar(mut view: View<i32>) -> Update<i32> {
///     // view can be dereferenced into the given type
///     // and is guaranteed to exist at this point
///     if *view < 5 {
///         // do something
///     }
///
///     with_io(view, |view| async {
///         // do something with `view` at runtime
///         Ok(view)
///     })
/// }
///
/// let worker: Worker<SystemState, Ready> = Worker::new()
///     .job("/{foo}/{bar}", update(foo_bar))
///     .initial_state(SystemState {/* ... */})
///     .unwrap();
/// ```
///
/// # Errors
///
/// Initializing the extractor will fail if the path assigned to the job cannot be resolved (or it
/// resolves to `Null`) or the value pointed by the path cannot be deserialized into type `<T>`
#[derive(Debug)]
pub struct View<T>(Pointer<T>);

impl<T: DeserializeOwned> FromSystem for View<T> {
    type Error = ExtractionError;

    fn from_system(system: &System, context: &Context) -> Result<Self, Self::Error> {
        let pointer = Pointer::<T>::from_system(system, context)?;

        // Fail if pointer is null
        if pointer.is_none() {
            return Err(anyhow!("Path {} does not exist", context.path).into());
        }

        Ok(View(pointer))
    }
}

impl<T> Deref for View<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // we can unwrap here because we know the state is not None
        self.0.state.as_ref().unwrap()
    }
}

impl<T> DerefMut for View<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // we can unwrap here because we know the state is not None
        self.0.state.as_mut().unwrap()
    }
}

impl<T: Serialize> IntoResult<Patch> for View<T> {
    fn into_result(self) -> Result<Patch, Error> {
        self.0.into_result()
    }
}

/// Convert a simple view into an effect
impl<T, E> From<View<T>> for Effect<View<T>, E> {
    fn from(view: View<T>) -> Effect<View<T>, E> {
        Effect::from_result(Ok(view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::System;
    use json_patch::Patch;
    use pretty_assertions::assert_eq;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::collections::HashMap;

    #[derive(Serialize, Deserialize, Debug)]
    struct State {
        numbers: HashMap<String, i32>,
    }

    #[derive(Serialize, Deserialize)]
    struct StateVec {
        numbers: Vec<String>,
    }

    #[test]
    fn it_extracts_an_existing_value_using_a_pointer() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<i32> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/one")).unwrap();

        assert_eq!(ptr.as_ref(), Some(&1));

        let value = ptr.as_mut().unwrap();
        *value = 2;

        // Get the list changes to the view

        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "replace", "path": "/numbers/one", "value": 2 },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_fails_if_pointed_path_is_invalid() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        assert!(Pointer::<i32>::from_system(
            &system,
            &Context::new().with_path("/numbers/one/two"),
        )
        .is_err());
        assert!(
            Pointer::<i32>::from_system(&system, &Context::new().with_path("/none/two"),).is_err()
        );
    }

    #[test]
    fn it_assigns_a_value_to_pointed_path() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<i32> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/three")).unwrap();

        assert_eq!(ptr.as_ref(), None);

        ptr.assign(3);

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "add", "path": "/numbers/three", "value": 3 },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_allows_changing_a_value_with_a_view() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let mut view: View<i32> =
            View::from_system(&system, &Context::new().with_path("/numbers/two")).unwrap();
        *view = 3;

        // Get the list changes to the view
        let changes = view.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "replace", "path": "/numbers/two", "value": 3 },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_fails_to_initialize_view_if_path_does_not_exist() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        assert!(
            View::<i32>::from_system(&system, &Context::new().with_path("/numbers/three")).is_err()
        );
        assert!(
            View::<i32>::from_system(&system, &Context::new().with_path("/none/three")).is_err()
        );
    }

    #[test]
    fn it_initializes_a_value_with_default() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<i32> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/three")).unwrap();

        assert_eq!(ptr.as_ref(), None);

        let value = ptr.zero();
        *value = 3;

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "add", "path": "/numbers/three", "value": 3 },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_deletes_an_existing_value() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<i32> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/one")).unwrap();

        // Delete the value
        ptr = ptr.unassign();

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "remove", "path": "/numbers/one" },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_extracts_an_existing_value_on_a_vec() {
        let state = StateVec {
            numbers: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<String> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/1")).unwrap();

        assert_eq!(ptr.as_ref(), Some(&"two".to_string()));

        let value = ptr.as_mut().unwrap();
        *value = "TWO".to_string();

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "replace", "path": "/numbers/1", "value": "TWO" },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_creates_a_value_on_a_vec() {
        let state = StateVec {
            numbers: vec!["one".to_string(), "two".to_string()],
        };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<String> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/2")).unwrap();

        assert_eq!(ptr.as_ref(), None);
        ptr.assign("three");

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "add", "path": "/numbers/2", "value": "three" },
            ]))
            .unwrap()
        );
    }

    #[test]
    fn it_deletes_a_value_on_a_vec() {
        let state = StateVec {
            numbers: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        };

        let mut system = System::try_from(state).unwrap();

        let mut ptr: Pointer<String> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/1")).unwrap();

        // Remove the second element
        ptr = ptr.unassign();

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            // Removing a value from the middle of the array requires shifting the indexes
            serde_json::from_value::<Patch>(json!([
              { "op": "remove", "path": "/numbers/1" },
            ]))
            .unwrap()
        );

        system.patch(changes).unwrap();
        assert_eq!(
            system.root(),
            &serde_json::from_value::<Value>(json!({"numbers": ["one", "three"]})).unwrap()
        );
    }

    #[test]
    fn it_deletes_a_value_from_the_end_of_a_vec() {
        let state = StateVec {
            numbers: vec!["one".to_string(), "two".to_string(), "three".to_string()],
        };

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<String> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/2")).unwrap();

        // Remove the third element
        ptr.take();

        // Get the list changes to the view
        let changes = ptr.into_result().unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "remove", "path": "/numbers/2" },
            ]))
            .unwrap()
        );
    }
}

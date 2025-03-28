use anyhow::{anyhow, Context as AnyhowCtx};
use json_patch::{diff, Patch};
use jsonptr::resolve::ResolveError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::ops::{Deref, DerefMut};

use crate::path::Path;
use crate::system::{FromSystem, System};
use crate::task::{Context, Effect, Error, InputError, IntoEffect, IntoResult, UnexpectedError};

/// Extracts a pointer to a sub-element of the global state indicated
/// by the path.
///
/// The type of the sub-element is given by the type parameter T.
///
/// The pointer can be null, meaning the parent of the element exists,
/// but the specific location pointed by the path does not exist.
#[derive(Debug)]
pub struct Pointer<T> {
    state: Option<T>,
    path: Path,
}

impl<T> Pointer<T> {
    // The only way to create a pointer is via the
    // from_system method
    fn new(state: Option<T>, path: Path) -> Self {
        Pointer { state, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Assign a value to location indicated by
    /// the path.
    pub fn assign(&mut self, value: impl Into<T>) -> &mut T {
        self.state = Some(value.into());
        self.state.as_mut().unwrap()
    }

    /// Clear the value at the location indicated by
    /// the path
    pub fn unassign(&mut self) {
        self.state = None;
    }

    /// Initialize the location pointed by the path
    /// with the defaut value of the type T.
    pub fn zero(&mut self) -> &mut T
    where
        T: Default,
    {
        self.assign(T::default())
    }
}

impl<T: DeserializeOwned> FromSystem for Pointer<T> {
    type Error = InputError;

    fn from_system(system: &System, context: &Context) -> Result<Self, Self::Error> {
        let json_ptr = context.path.as_ref();
        let root = system.root();

        // Use the parent of the pointer unless we are at the root
        let parent = json_ptr.parent().unwrap_or(json_ptr);

        // Try to resolve the parent or fail
        parent
            .resolve(root)
            .with_context(|| format!("Failed to resolve path {}", context.path))?;

        // At this point we assume that if the pointer cannot be
        // resolved is because the value does not exist yet unless
        // the parent is a scalar
        let state: Option<T> = match json_ptr.resolve(root) {
            Ok(value) => Some(serde_json::from_value::<T>(value.clone()).with_context(|| {
                format!(
                    "Failed to deserialize {value} into {}",
                    std::any::type_name::<T>()
                )
            })?),
            Err(e) => match e {
                ResolveError::NotFound { .. } => None,
                ResolveError::OutOfBounds { .. } => None,
                _ => {
                    return Err(
                        anyhow!(e).context(format!("Failed to resolve path {}", context.path))
                    )?;
                }
            },
        };

        Ok(Pointer::new(state, context.path.clone()))
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

impl<T: Serialize> IntoResult<Patch> for Pointer<T> {
    fn into_result(self, system: &System) -> Result<Patch, Error> {
        // Get the root value
        let mut after = system.clone();
        let root = after.root_mut();

        let json_ptr = self.path.as_ref();

        if let Some(state) = self.state {
            let value = serde_json::to_value(state)
                .with_context(|| "Failed to serialize pointer value")
                .map_err(UnexpectedError::from)?;

            // Assign the state to the copy
            json_ptr
                .assign(root, value)
                .with_context(|| format!("Failed to assign path {}", self.path))
                .map_err(UnexpectedError::from)?;
        } else {
            // Otherwise delete the path at the pointer
            json_ptr.delete(root);
        }
        Ok(diff(system.root(), root))
    }
}

// Allow tasks to return a pointer
// This converts the pointer into a pure effect
impl<T: Serialize> IntoEffect<Patch, Error> for Pointer<T> {
    fn into_effect(self, system: &System) -> Effect<Patch, Error> {
        Effect::from(self.into_result(system))
    }
}

/// Extracts a sub-element of a state S as indicated by
/// a path.
///
/// Differently from Pointer, the View presumes the location
/// pointed by the value exists and extraction will fail otherwise
#[derive(Debug)]
pub struct View<T>(Pointer<T>);

impl<T> View<T> {
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl<T: DeserializeOwned> FromSystem for View<T> {
    type Error = InputError;

    fn from_system(system: &System, context: &Context) -> Result<Self, Self::Error> {
        let pointer = Pointer::<T>::from_system(system, context)?;

        if pointer.state.is_none() {
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
    fn into_result(self, system: &System) -> Result<Patch, Error> {
        self.0.into_result(system)
    }
}

// Allow tasks to return a view
// This converts the view into a pure effect
impl<T: Serialize> IntoEffect<Patch, Error> for View<T> {
    fn into_effect(self, system: &System) -> Effect<Patch, Error> {
        Effect::from(self.into_result(system))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::System;
    use json_patch::Patch;
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

        let changes = ptr.into_result(&system).unwrap();
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
        let changes = ptr.into_result(&system).unwrap();
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
        let changes = view.into_result(&system).unwrap();
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
        let changes = ptr.into_result(&system).unwrap();
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
        ptr.unassign();

        // Get the list changes to the view
        let changes = ptr.into_result(&system).unwrap();
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
        let changes = ptr.into_result(&system).unwrap();
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
        let changes = ptr.into_result(&system).unwrap();
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

        let system = System::try_from(state).unwrap();

        let mut ptr: Pointer<String> =
            Pointer::from_system(&system, &Context::new().with_path("/numbers/1")).unwrap();

        // Remove the second element
        ptr.unassign();

        // Get the list changes to the view
        let changes = ptr.into_result(&system).unwrap();
        assert_eq!(
            changes,
            // Removing a value from the middle of the array requires shifting the indexes
            serde_json::from_value::<Patch>(json!([
              { "op": "replace", "path": "/numbers/1", "value": "three" },
              { "op": "remove", "path": "/numbers/2" },
            ]))
            .unwrap()
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
        ptr.unassign();

        // Get the list changes to the view
        let changes = ptr.into_result(&system).unwrap();
        assert_eq!(
            changes,
            serde_json::from_value::<Patch>(json!([
              { "op": "remove", "path": "/numbers/2" },
            ]))
            .unwrap()
        );
    }
}

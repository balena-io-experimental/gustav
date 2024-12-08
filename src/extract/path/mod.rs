use serde::de::DeserializeOwned;
use std::ops::Deref;

use crate::error::{Error, IntoError};
use crate::system::{FromSystem, System};
use crate::task::Context;

mod de;
mod error;

pub use error::PathDeserializationError;

impl IntoError for PathDeserializationError {
    fn into_error(self) -> Error {
        Error::PathExtractFailed(self)
    }
}

#[derive(Debug)]
pub struct Path<T>(pub T);

impl<S, T: DeserializeOwned + Send> FromSystem<S> for Path<T> {
    type Error = PathDeserializationError;

    fn from_system(_: &System, context: &Context<S>) -> Result<Self, Self::Error> {
        let args = &context.args;
        T::deserialize(de::PathDeserializer::new(args)).map(Path)
    }
}

impl<S> Deref for Path<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    // The state model
    #[derive(Serialize, Deserialize, Debug, Clone)]
    struct State {
        numbers: HashMap<String, i32>,
    }

    #[test]
    fn deserializes_simple_path_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::from(state);

        let Path(name): Path<String> =
            Path::from_system(&system, &Context::<State>::new().arg("name", "one")).unwrap();

        assert_eq!(name, "one");
    }

    #[test]
    fn deserializes_tuple_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::from(state);

        let Path((first, second)): Path<(String, String)> = Path::from_system(
            &system,
            &Context::<State>::new()
                .arg("first", "one")
                .arg("second", "two"),
        )
        .unwrap();

        assert_eq!(first, "one");
        assert_eq!(second, "two");
    }

    #[test]
    fn deserializes_hashmap_args() {
        let mut numbers = HashMap::new();
        numbers.insert("one".to_string(), 1);
        numbers.insert("two".to_string(), 2);

        let state = State { numbers };

        let system = System::from(state);

        let Path(map): Path<HashMap<String, String>> = Path::from_system(
            &system,
            &Context::<State>::new()
                .arg("first", "one")
                .arg("second", "two"),
        )
        .unwrap();

        assert_eq!(
            map,
            HashMap::from([
                ("first".to_string(), "one".to_string()),
                ("second".to_string(), "two".to_string())
            ])
        );
    }
}

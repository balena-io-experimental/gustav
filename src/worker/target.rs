use json_patch::{diff, Patch, PatchOperation, RemoveOperation, ReplaceOperation};
use jsonptr::Pointer;
use serde::Serialize;
use serde_json::Value;
use std::{
    cmp::Ordering,
    collections::{BTreeSet, LinkedList},
    fmt::{self, Display},
    ops::Deref,
};

pub(crate) struct Distance(BTreeSet<Operation>);

impl Distance {
    fn new() -> Self {
        Distance(BTreeSet::new())
    }

    fn insert(&mut self, o: Operation) {
        self.0.insert(o);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Operation> {
        self.0.iter()
    }

    fn add_removes_for_children(&mut self, path: &Pointer, value: &Value) {
        let mut queue = LinkedList::new();
        queue.push_back((path.to_buf(), value));

        while let Some((path, value)) = queue.pop_front() {
            if value.is_object() {
                let obj = value.as_object().unwrap();
                for (k, v) in obj.iter() {
                    let path = path.concat(Pointer::parse(&format!("/{}", k)).unwrap());
                    // Insert a remove operation for each child
                    self.insert(Operation::from(PatchOperation::Remove(RemoveOperation {
                        path: path.clone(),
                    })));

                    // Append the value to the queue
                    queue.push_back((path, v));
                }
            }

            if value.is_array() {
                let obj = value.as_array().unwrap();
                for (k, v) in obj.iter().enumerate() {
                    let path = path.concat(Pointer::parse(&format!("/{}", k)).unwrap());

                    // Insert a remove operation for each child
                    self.insert(Operation::from(PatchOperation::Remove(RemoveOperation {
                        path: path.clone(),
                    })));

                    // Append the value to the queue
                    queue.push_back((path, v));
                }
            }
        }
    }
}

pub struct Target(Value);

#[derive(Debug)]
pub enum TargetError {
    SerializationFailed(serde_json::error::Error),
}

impl std::error::Error for TargetError {}

impl Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetError::SerializationFailed(err) => err.fmt(f),
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub(crate) struct Operation(PatchOperation);

impl PartialOrd for Operation {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Operation {
    fn cmp(&self, other: &Self) -> Ordering {
        let thispath = self.0.path();
        let otherpath = other.0.path();

        // Order operations by path length
        thispath
            .count()
            .cmp(&otherpath.count())
            .then(thispath.cmp(otherpath))
    }
}

impl From<PatchOperation> for Operation {
    fn from(op: PatchOperation) -> Operation {
        Operation(op)
    }
}

impl Target {
    pub fn new(target: impl Into<Value>) -> Self {
        Self(target.into())
    }

    pub fn from<S: Serialize>(state: S) -> Result<Self, TargetError> {
        let state = serde_json::to_value(state).map_err(TargetError::SerializationFailed)?;
        Ok(Target::new(state))
    }

    #[allow(dead_code)]
    /// Calculate the distance between some state and the self target
    ///
    /// The distance is the list of operations required in order to convert the
    /// the state into the target. The distance also includes alternate paths
    ///
    /// For instance if the target creates a new value at the
    /// pointer `/a/b/c`, that new value could come from a create operation on the
    /// given pointer, but also from an update operation on `/a/b` or `/a` or `/`.
    ///
    /// Similarly, in order to delete a pointer `/a/b`, just removing the path could be enough, but
    /// it might be necessary to remove every child of `/a/b` before being able to remove the
    /// path.
    ///
    /// The distance encodes all the possible operations that can be used to move
    /// between two states
    pub(crate) fn distance(&self, state: &Value) -> Distance {
        let mut distance = Distance::new();

        // calculate differences between the system root and
        // the target
        let Patch(changes) = diff(state, self);
        for op in changes {
            // For every operation on the list of changes
            let path = op.path();

            let mut parent = path;

            // get all paths up to the root
            while let Some(newparent) = parent.parent() {
                // get the target at the parent to use as value
                // no matter the operation, the parent of the target shoul
                // always exist. If it doesn't there is a bug (probbly in jsonptr)
                let value = newparent.resolve(&self.0).unwrap_or_else(|e| {
                    panic!(
                        "[BUG] Path `{}` should be resolvable on the target, but got error: {}",
                        newparent, e
                    )
                });

                // Insert a replace operation for each one
                distance.insert(Operation::from(PatchOperation::Replace(ReplaceOperation {
                    path: newparent.to_buf(),
                    value: value.clone(),
                })));

                parent = newparent;
            }

            // for every delete operation '/a/b/c', add child
            // nodes of the deleted path with 'remove' operation
            if let PatchOperation::Remove(_) = op {
                // By definition of the remove operation the path should be
                // resolvable on the left side of the diff
                let value = path.resolve(state).unwrap_or_else(|e| {
                    panic!(
                        "[BUG] Path `{}` should be resolvable on the state, but got error: {}",
                        path, e
                    )
                });

                //
                distance.add_removes_for_children(path, value);
            }

            // Finally insert the actual operation
            distance.insert(Operation::from(op));
        }

        distance
    }
}

impl Deref for Target {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn distance_eq(src: Value, tgt: Value, result: Vec<Value>) {
        let target = Target::new(tgt);
        let distance = target.distance(&src);

        // Serialize results to make comparison easier
        let ops: Vec<Value> = distance
            .iter()
            .map(|Operation(o)| serde_json::to_value(o).unwrap())
            .collect();

        assert_eq!(ops, result)
    }

    #[test]
    fn it_calculates_possible_changes_to_target() {
        distance_eq(
            json!({"a": 1, "b": "one", "c": {"k": "v"}}),
            json!({"a": 2, "b": "one", "c": {}}),
            vec![
                json!({"op": "replace", "path": "", "value": {"a": 2, "b": "one", "c": {}}}),
                json!({"op": "replace", "path": "/a", "value": 2}),
                json!({"op": "replace", "path": "/c", "value": {}}),
                json!({"op": "remove", "path": "/c/k"}),
            ],
        );
        distance_eq(
            json!({"a": 1, "b": "one", "c": {"k": "v"}}),
            json!({"a": 2}),
            vec![
                json!({"op": "replace", "path": "", "value": {"a": 2}}),
                json!({"op": "replace", "path": "/a", "value": 2}),
                json!({"op": "remove", "path": "/b"}),
                json!({"op": "remove", "path": "/c"}),
                json!({"op": "remove", "path": "/c/k"}),
            ],
        );
        distance_eq(
            json!({"a": 1, "b": "one", "c": {"k": "v"}}),
            json!({"a": 2, "b": "two", "c": {"k": "v"}}),
            vec![
                json!({"op": "replace", "path": "", "value": {"a": 2, "b": "two", "c": {"k": "v"}}}),
                json!({"op": "replace", "path": "/a", "value": 2}),
                json!({"op": "replace", "path": "/b", "value": "two"}),
            ],
        );
        distance_eq(
            json!({"a": {"b": {"c": {"d": "e"}}}}),
            json!({"a": {"b": {}}}),
            vec![
                json!({"op": "replace", "path": "", "value": {"a": {"b": {}}}}),
                json!({"op": "replace", "path": "/a", "value": {"b": {}}}),
                json!({"op": "replace", "path": "/a/b", "value": {}}),
                json!({"op": "remove", "path": "/a/b/c"}),
                json!({"op": "remove", "path": "/a/b/c/d"}),
            ],
        );
    }

    #[test]
    fn it_allows_filtering_some_paths_from_calculation() {
        // TODO
        // we not only need to calculate the operations to the target but we need to be able
        // to ignore some elements from the difference. This is so we can use the same structure
        // for both current and target state. For instance, there might be a runtime value that
        // encodes the creation date of an entity, but we may not really want to compare based on
        // creation date when calculating the distance
    }
}

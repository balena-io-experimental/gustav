use jsonptr::{Pointer, PointerBuf};
use std::fmt::Display;
use std::ops::Deref;
use std::sync::Arc;

#[derive(Clone, Default, PartialEq, Debug)]
pub struct Path(PointerBuf);

impl Path {
    pub(crate) fn new(pointer: &Pointer) -> Self {
        Self(pointer.to_buf())
    }

    /// Create a static path from a string
    ///
    /// # Panics
    ///
    /// This will panic if the string is not a valid path
    pub fn from_static(s: &'static str) -> Self {
        Path(Pointer::from_static(s).to_buf())
    }

    pub fn to_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Path> for String {
    fn from(path: Path) -> String {
        path.0.to_string()
    }
}

impl AsRef<Pointer> for Path {
    fn as_ref(&self) -> &Pointer {
        &self.0
    }
}

// Structure to store path arguments when matching
// against a lens
#[derive(Clone, Default, Debug)]
pub(crate) struct PathArgs(pub Vec<(Arc<str>, String)>);

impl PathArgs {
    pub fn iter(&self) -> impl Iterator<Item = &(Arc<str>, String)> {
        self.0.iter()
    }
}

impl Deref for PathArgs {
    type Target = Vec<(Arc<str>, String)>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<matchit::Params<'_, '_>> for PathArgs {
    fn from(params: matchit::Params) -> PathArgs {
        let params: Vec<(Arc<str>, String)> = params
            .iter()
            .map(|(k, v)| (Arc::from(k), String::from(v)))
            .collect();

        PathArgs(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_converts_a_path_to_string() {
        assert_eq!(Path::from_static("/a/b/c").to_string(), "/a/b/c");
        assert_eq!(Path::from_static("/").to_string(), "/");
        assert_eq!(Path::from_static("").to_string(), "");
    }

    #[test]
    fn it_converts_a_str_to_a_path() {
        let path = Path::from_static("/a/b/c");
        assert_eq!(String::from(path), "/a/b/c");
    }

    #[test]
    #[should_panic]
    fn it_panics_on_an_invalid_path() {
        Path::from_static("a/b/c");
    }
}

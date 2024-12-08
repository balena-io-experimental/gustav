use std::sync::Arc;

use crate::path::{Path, PathArgs};

pub struct Context<S> {
    pub(crate) target: Option<S>,
    pub(crate) path: Path,
    pub(crate) args: PathArgs,
}

impl<S> Default for Context<S> {
    fn default() -> Self {
        Context {
            target: None,
            path: Path::default(),
            args: PathArgs::new(),
        }
    }
}

impl<S> Context<S> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn target(self, target: S) -> Self {
        Self {
            target: Some(target),
            path: self.path,
            args: self.args,
        }
    }

    // This will be used by the planner
    #[allow(dead_code)]
    pub(crate) fn path(self, path: &'static str) -> Self {
        Self {
            target: self.target,
            path: Path::from_static(path),
            args: self.args,
        }
    }

    pub fn arg(self, key: impl AsRef<str>, value: impl Into<String>) -> Self {
        let Self {
            target,
            path,
            mut args,
        } = self;

        args.0.push((Arc::from(key.as_ref()), value.into()));

        Self { target, path, args }
    }
}

impl<S: Clone> Clone for Context<S> {
    fn clone(&self) -> Self {
        Context {
            target: self.target.clone(),
            path: self.path.clone(),
            args: self.args.clone(),
        }
    }
}

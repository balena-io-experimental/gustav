use crate::{error::Error, system::System};

/// The task outcome is a type alias
/// of Result
pub type Result<O> = core::result::Result<O, Error>;

pub trait IntoResult<O> {
    fn into_result(self, system: &System) -> Result<O>;
}

impl<T, E, O> IntoResult<O> for core::result::Result<T, E>
where
    T: IntoResult<O>,
    E: std::error::Error + Sync + Send + 'static,
{
    fn into_result(self, system: &System) -> Result<O> {
        match self {
            Ok(value) => value.into_result(system),
            Err(e) => Err(Error::Other(Box::new(e))),
        }
    }
}

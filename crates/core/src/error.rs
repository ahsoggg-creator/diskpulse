use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Win32 `ERROR_ACCESS_DENIED`.
pub const ERROR_ACCESS_DENIED: u32 = 5;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("{op} failed (Windows error {code})")]
    Os { op: &'static str, code: u32 },
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    Unsupported(String),
}

impl Error {
    /// The raw device could not be opened / queried because the process is not elevated.
    pub fn is_access_denied(&self) -> bool {
        matches!(self, Error::Os { code, .. } if *code == ERROR_ACCESS_DENIED)
    }
}

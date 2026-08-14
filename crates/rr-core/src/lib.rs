#![deny(unsafe_code)]

pub mod lang;
pub mod path;
pub mod walk;

pub use lang::Lang;
pub use path::{RelPath, RelPathError};
pub use walk::{discover, is_generated, SourceFile, WalkCfg, DEFAULT_EXCLUDES};

/// Core error types for `rr-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ignore error: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("Invalid relative path: {0}")]
    InvalidRelPath(#[from] RelPathError),
}

/// A specialized [`Result`](std::result::Result) type for `rr-core` operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_result_type() {
        let ok_result: Result<&str> = Ok("success");
        assert!(ok_result.is_ok());
    }
}

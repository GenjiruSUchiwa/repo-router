#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used)]

//! Git integration for `repo-router`.
//!
//! Provides fast Git object identifier computation, Git blob hashing, repository discovery,
//! and index-based OID resolution with zero content reads on clean files.

pub mod content;
pub mod map;
pub mod oid;
pub mod repo;

pub use content::{acquire_non_git, AcquiredContent, ContentProbe, ContentRepresentation};
pub use map::build_map;
pub use oid::{hash_blob, HashAlgo, Oid, OidError};
pub use repo::{oid_of, GitRepo};

/// Git error types for `rr-git`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Git repository discovery failure.
    #[error("Git discovery error: {0}")]
    Discover(#[from] Box<gix::discover::Error>),
    /// Git index reading or opening failure.
    #[error("Git index error: {0}")]
    Index(#[from] Box<gix::worktree::open_index::Error>),
    #[error("core error: {0}")]
    Core(#[from] rr_core::Error),
    #[error("content acquisition failed: {0}")]
    Content(String),
    /// Git object identifier validation failure.
    #[error("OID error: {0}")]
    Oid(#[from] OidError),
}

impl From<gix::discover::Error> for Error {
    fn from(err: gix::discover::Error) -> Self {
        Self::Discover(Box::new(err))
    }
}

impl From<gix::worktree::open_index::Error> for Error {
    fn from(err: gix::worktree::open_index::Error) -> Self {
        Self::Index(Box::new(err))
    }
}

/// A specialized [`Result`](std::result::Result) type for `rr-git` operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_git_result_type() {
        let ok_result: Result<&str> = Ok("success");
        assert!(ok_result.is_ok());
    }
}

#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used)]

//! Git integration for `repo-router`.
//!
//! Provides fast Git object identifier computation, Git blob hashing, repository discovery,
//! and index-based OID resolution with zero content reads on clean files.

pub mod content;
pub mod diff;
pub mod guard;
pub mod map;
pub mod oid;
pub mod pipeline;
pub mod plan;
pub mod refresh;
pub mod repo;
pub mod rules;
mod safe_open;
mod sigpipe;

pub mod status;

pub use content::{
    acquire_for_source, acquire_non_git, revalidate_source, AcquireOutcome, AcquiredContent,
    ContentProbe, ContentRepresentation,
};
pub use diff::{change_set, ChangeSet, ChangeTarget, FileDelta, Hunk};
pub use guard::{release_locks_signal_safe, RepositoryWriteGuard};
pub use map::build_map;
pub use oid::{hash_blob, HashAlgo, Oid, OidError};
pub use plan::commits_left_index_true;
pub use refresh::{hold, prepare, refresh, HeldSnapshot, PreparedRefresh, Refresh};
pub use repo::{oid_of, ChangeKind, GitRepo, HeadState, RepoState, WorktreeChange};
pub use status::status;

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
    /// Reading, encoding, or publishing the snapshot file failed.
    #[error("snapshot error: {0}")]
    Snapshot(#[from] rr_core::snapshot::SnapshotIoError),
    #[error("content acquisition failed: {0}")]
    Content(String),
    /// The caller asked to stop before the work finished.
    #[error("cancelled")]
    Cancelled,
    /// Another process is already publishing for this repository.
    #[error("another rr process is refreshing this repository ({})", path.display())]
    PublicationLocked { path: std::path::PathBuf },
    /// The repository moved while the snapshot was being built, so the snapshot
    /// no longer describes it and was not published.
    #[error("the repository changed while refreshing; nothing was published")]
    RepositoryChanged,
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

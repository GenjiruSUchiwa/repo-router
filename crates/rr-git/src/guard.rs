//! Exclusive right to publish a snapshot for one repository.
//!
//! Two refreshes running at once would each read the current snapshot, each
//! build a successor from it, and each publish — and the second would erase the
//! first's work while reporting success. The rename that publishes is atomic, so
//! no reader ever sees a torn file; what needs protecting is the read-decide-write
//! sequence around it, and that is what this guard covers.

use std::path::{Path, PathBuf};

use rr_core::workspace;

use crate::{Error, Result};

/// Holds the sole right to publish, releasing it when dropped.
///
/// Acquisition fails immediately rather than waiting. A refresh is a read-mostly
/// operation a user runs interactively or an editor runs on save; a caller that
/// blocks behind another one produces a queue of redundant rebuilds of a
/// repository the winner has already described. Failing tells the caller the
/// truth — someone else is doing this — and lets it decide.
#[derive(Debug)]
pub struct RepositoryWriteGuard {
    // The marker owns the lock file and removes it on drop. Nothing reads it:
    // its existence *is* the claim, which is why it holds no contents. A PID or
    // a timestamp inside would only invite the next reader to guess whether the
    // holder is still alive, and a guess that is wrong deletes a live lock.
    _marker: gix_lock::Marker,
    path: PathBuf,
}

impl RepositoryWriteGuard {
    /// Claims the right to publish for the repository rooted at `root`.
    ///
    /// # Errors
    /// Returns [`Error::PublicationLocked`] when another process holds the
    /// claim, and [`Error::Io`] when the lock directory cannot be created.
    pub fn acquire(root: &Path) -> Result<Self> {
        // The lock lives beside the snapshot it protects, so the directory has
        // to exist before the claim can. Creating it here also stamps the ignore
        // rule, which matters on a first run that never reaches a build.
        workspace::ensure_private(root).map_err(Error::Io)?;
        let local = workspace::local_dir(root);
        std::fs::create_dir_all(&local).map_err(Error::Io)?;

        let path = workspace::publication_lock_path(root);
        let marker = gix_lock::Marker::acquire_to_hold_resource(
            &path,
            gix_lock::acquire::Fail::Immediately,
            None,
        )
        .map_err(|_| Error::PublicationLocked { path: path.clone() })?;

        Ok(Self {
            _marker: marker,
            path,
        })
    }

    /// The lock file backing this claim, for diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_fails_immediately_rather_than_waiting() {
        let temp = tempfile::tempdir().unwrap();
        let first = RepositoryWriteGuard::acquire(temp.path()).unwrap();

        let second = RepositoryWriteGuard::acquire(temp.path());
        assert!(
            matches!(second, Err(Error::PublicationLocked { .. })),
            "expected a refusal, got {second:?}",
        );
        drop(first);
    }

    #[test]
    fn releasing_the_claim_lets_the_next_caller_take_it() {
        let temp = tempfile::tempdir().unwrap();
        drop(RepositoryWriteGuard::acquire(temp.path()).unwrap());
        RepositoryWriteGuard::acquire(temp.path()).expect("the claim was released");
    }

    #[test]
    fn claiming_creates_the_state_directory_it_needs() {
        let temp = tempfile::tempdir().unwrap();
        let guard = RepositoryWriteGuard::acquire(temp.path()).unwrap();

        assert!(workspace::local_dir(temp.path()).is_dir());
        assert!(
            workspace::state_dir(temp.path())
                .join(".gitignore")
                .exists(),
            "a first run that only locks must still hide what it created"
        );
        drop(guard);
    }

    /// Two repositories are two independent claims. Sharing one would make a
    /// refresh in one checkout block a refresh in an unrelated one.
    #[test]
    fn claims_on_different_repositories_do_not_collide() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();

        let _first = RepositoryWriteGuard::acquire(one.path()).unwrap();
        RepositoryWriteGuard::acquire(two.path()).expect("a different repository");
    }
}

//! The cleanup a terminating signal gets in place of the `Drop` it pre-empted.
//!
//! One test, alone in its binary on purpose. `release_locks_signal_safe` works
//! on the whole process — it is the last act of one that is dying, so it takes
//! every lock with it and asks nobody. Sharing a process with tests that hold
//! locks of their own would let it delete theirs mid-assertion.

#![allow(clippy::unwrap_used)]

use rr_git::{release_locks_signal_safe, RepositoryWriteGuard};

#[test]
fn the_signal_safe_release_removes_a_claim_that_is_still_held() {
    let temp = tempfile::tempdir().unwrap();
    let guard = RepositoryWriteGuard::acquire(temp.path()).unwrap();
    let lock = guard.path().with_extension("lock");
    assert!(lock.exists(), "the claim is not on disk to begin with");
    release_locks_signal_safe();

    assert!(
        !lock.exists(),
        "a held claim outlived the release, so a signalled run would refuse every later one"
    );
    drop(guard);
    RepositoryWriteGuard::acquire(temp.path()).expect("the claim was released");
}

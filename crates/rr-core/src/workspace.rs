//! The layout of the tool's own state directory, and the fact that it is private.
//!
//! Everything `rr` writes lives under one directory in the repository root. Two
//! things follow from that, and both are settled here rather than at each write
//! site: where a given artifact goes, and that Git must never see any of it.
//!
//! The second is not a detail. `.rr` holds one snapshot and a fact cache with an
//! entry per file per version — thousands of files that Git would otherwise
//! enumerate and report as untracked on every status. A refresh that sees its
//! own previous output as a repository change can never conclude that nothing
//! happened, so the cheapest and most important state — "no work to do" —
//! becomes unreachable. Marking the directory ignored is what makes it reachable.

use std::path::{Path, PathBuf};

/// The directory holding everything this tool writes.
pub const STATE_DIR: &str = ".rr";

/// The subdirectory for machine-local artifacts, which are never shared.
pub const LOCAL_DIR: &str = "local";

/// The ignore rule stamped into the state directory.
///
/// A single `*` inside `.rr/.gitignore` prunes the whole subtree, and does so
/// for Git itself, for `gix`, and for the `ignore` crate the walker uses — one
/// file that every consumer already knows how to read, instead of a private
/// exclusion rule each of them would have to be taught separately.
const IGNORE_EVERYTHING: &str =
    "# Written by rr. Everything here is generated and machine-local.\n*\n";

/// `<root>/.rr`
#[must_use]
pub fn state_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR)
}

/// `<root>/.rr/local`
#[must_use]
pub fn local_dir(root: &Path) -> PathBuf {
    state_dir(root).join(LOCAL_DIR)
}

/// `<root>/.rr/local/facts`
#[must_use]
pub fn facts_dir(root: &Path) -> PathBuf {
    local_dir(root).join("facts")
}

/// `<root>/.rr/local/snapshot.bin`
#[must_use]
pub fn snapshot_path(root: &Path) -> PathBuf {
    local_dir(root).join("snapshot.bin")
}

/// `<root>/.rr/local/publication`, the lock that serializes publishers.
#[must_use]
pub fn publication_lock_path(root: &Path) -> PathBuf {
    local_dir(root).join("publication")
}

/// Creates the state directory and marks it ignored.
///
/// Called before writing anything under `.rr`, so the mark exists before the
/// files it hides do. Re-stamping a directory that already carries the rule is
/// skipped, which keeps the common path free of writes; a rule someone edited
/// by hand is left alone, because a user who put something there meant it.
///
/// # Errors
/// Returns the underlying I/O error when the directory cannot be created. A
/// failure to write the ignore rule itself is not an error: the rule is an
/// optimization for a directory that is already excluded by name, and a
/// read-only checkout should still be able to run.
pub fn ensure_private(root: &Path) -> std::io::Result<()> {
    let dir = state_dir(root);
    std::fs::create_dir_all(&dir)?;

    let stamp = dir.join(".gitignore");
    if !stamp.exists() {
        let _ = std::fs::write(&stamp, IGNORE_EVERYTHING);
    }
    Ok(())
}

/// Whether a repository-relative path belongs to this tool rather than the user.
///
/// The ignore stamp is the mechanism that keeps these paths out of a status
/// scan; this predicate is what makes the intent checkable without depending on
/// a file the user can delete.
#[must_use]
pub fn is_private_path(rel: &str) -> bool {
    rel == STATE_DIR || rel.starts_with(concat!(".rr", "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artifact_lives_under_the_one_state_directory() {
        let root = Path::new("/repo");
        for path in [
            facts_dir(root),
            snapshot_path(root),
            publication_lock_path(root),
        ] {
            assert!(
                path.starts_with(state_dir(root)),
                "{} escaped the state directory",
                path.display()
            );
        }
    }

    #[test]
    fn the_state_directory_and_its_contents_are_private() {
        assert!(is_private_path(".rr"));
        assert!(is_private_path(".rr/local/snapshot.bin"));
        assert!(is_private_path(".rr/local/facts/ab/cdef.bin"));
    }

    #[test]
    fn a_path_that_merely_starts_with_the_same_letters_is_not_private() {
        assert!(!is_private_path(".rrules"));
        assert!(!is_private_path(".rr-backup/thing"));
        assert!(!is_private_path("src/.rr/thing"));
    }

    #[test]
    fn stamping_is_idempotent_and_does_not_overwrite_a_hand_written_rule() {
        let temp = tempfile::tempdir().expect("tempdir");
        ensure_private(temp.path()).expect("first stamp");

        let stamp = state_dir(temp.path()).join(".gitignore");
        std::fs::write(&stamp, "custom\n").expect("hand edit");
        ensure_private(temp.path()).expect("second stamp");

        assert_eq!(
            std::fs::read_to_string(&stamp).expect("read back"),
            "custom\n"
        );
    }
}

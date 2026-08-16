//! What two commits disagree about.
//!
//! The incremental delta is `git status` — the working tree against `HEAD` — so
//! until this module existed a snapshot was only usable while `HEAD` stood
//! still. Issue #11 made rr produce files whose whole purpose is to be
//! committed, so every generation is now followed by a `HEAD` move, and every
//! user paid a full walk after every generate-and-commit cycle. This module
//! answers the other half of the question — what two commits disagree about —
//! so that the answer can be composed with what the working tree says.
//!
//! Every failure here is a rebuild, never an error. A commit the object
//! database no longer has, a tree that will not decode, a path that is not
//! UTF-8: each ends in [`CommittedDelta::Rebuild`], which is exactly what
//! happened unconditionally before. That is what makes this safe to add — it
//! can replace a rebuild with a delta, and it can never turn a run that worked
//! into one that fails.

use gix::bstr::ByteSlice;
use gix::diff::tree::recorder::{Change, Location};

use rr_core::path::RelPath;
use rr_core::refresh::FullReason;

use crate::content::object_id;
use crate::oid::Oid;
use crate::rules::is_rule_path;

use super::GitRepo;

/// What one path did between two commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommittedKind {
    /// Present in the newer commit, and not as the older commit had it.
    Touched,
    /// Present in the older commit and gone from the newer one.
    Removed,
}

/// One path two commits disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedChange {
    /// What the path did.
    pub kind: CommittedKind,
    /// The repository-relative path.
    pub path: RelPath,
}

/// The comparison of two commits, or the reason there cannot be one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommittedDelta {
    /// Every path the two commits disagree about, in tree order.
    Paths(Vec<CommittedChange>),
    /// No delta is available, and this is why the caller must rebuild.
    Rebuild(FullReason),
}

impl GitRepo {
    /// What `before` and `after` disagree about, or why they cannot be compared.
    ///
    /// A committed rule file ends this as [`FullReason::DiscoveryRulesChanged`]
    /// rather than as a path. [`crate::rules::discovery_digest`] hashes only the
    /// rule files that are *dirty*, on the stated grounds that "a rule file that
    /// matches `HEAD` is already covered by the commit the snapshot records" —
    /// true only while a moved `HEAD` forces a rebuild by itself. Committing a
    /// `.gitignore` changes which paths exist without changing any of them, and
    /// no path-level delta can say that.
    #[must_use]
    pub fn committed_delta(&self, before: Oid, after: Oid) -> CommittedDelta {
        let repo = self.gix_repo();
        let (Ok(before), Ok(after)) = (object_id(before), object_id(after)) else {
            return CommittedDelta::Rebuild(FullReason::HeadChanged);
        };
        let (Some(old), Some(new)) = (tree_of(repo, before), tree_of(repo, after)) else {
            return CommittedDelta::Rebuild(FullReason::HeadChanged);
        };

        let mut recorder =
            gix::diff::tree::Recorder::default().track_location(Some(Location::Path));
        if gix::diff::tree(
            gix::objs::TreeRefIter::from_bytes(&old.data, repo.object_hash()),
            gix::objs::TreeRefIter::from_bytes(&new.data, repo.object_hash()),
            gix::diff::tree::State::default(),
            &repo.objects,
            &mut recorder,
        )
        .is_err()
        {
            return CommittedDelta::Rebuild(FullReason::HeadChanged);
        }

        let mut changes = Vec::with_capacity(recorder.records.len());
        for record in &recorder.records {
            let (kind, path) = match record {
                Change::Addition {
                    entry_mode, path, ..
                } => {
                    if !is_path_entry(*entry_mode) {
                        continue;
                    }
                    (CommittedKind::Touched, path)
                }
                Change::Deletion {
                    entry_mode, path, ..
                } => {
                    if !is_path_entry(*entry_mode) {
                        continue;
                    }
                    (CommittedKind::Removed, path)
                }
                Change::Modification {
                    previous_entry_mode,
                    entry_mode,
                    path,
                    ..
                } => {
                    if is_path_entry(*entry_mode) {
                        (CommittedKind::Touched, path)
                    } else if is_path_entry(*previous_entry_mode) {
                        (CommittedKind::Removed, path)
                    } else {
                        continue;
                    }
                }
            };
            let Ok(path) = path
                .to_str()
                .map_err(drop)
                .and_then(|path| RelPath::new(path).map_err(drop))
            else {
                return CommittedDelta::Rebuild(FullReason::HeadChanged);
            };
            if is_rule_path(&path) {
                return CommittedDelta::Rebuild(FullReason::DiscoveryRulesChanged);
            }
            changes.push(CommittedChange { kind, path });
        }
        CommittedDelta::Paths(changes)
    }
}

/// The tree of one commit, or `None` when this repository cannot produce it.
///
/// One `Option` rather than two error types: a shallow clone that never fetched
/// the commit, an object pruned out from under a stale snapshot, a name that is
/// not a commit at all — every one of them leads to the same conclusion, and
/// naming them apart would only offer the caller a choice it does not have.
fn tree_of(repo: &gix::Repository, id: gix::ObjectId) -> Option<gix::Tree<'_>> {
    repo.find_object(id).ok()?.peel_to_tree().ok()
}

/// Whether an entry is a path the index could hold.
///
/// Directories and submodule gitlinks are recorded alongside the blobs — 521
/// records for a commit `git diff --name-status` calls 473 files — and neither
/// is a path a snapshot holds. Blobs under a newly added directory are recorded
/// individually, so skipping the directory itself loses nothing.
fn is_path_entry(mode: gix::objs::tree::EntryMode) -> bool {
    !mode.is_tree() && !mode.is_commit()
}

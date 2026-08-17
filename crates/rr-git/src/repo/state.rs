//! One canonical observation of `HEAD`, the index, and the working tree.
//!
//! Everything downstream plans from this value and nothing else. Git reports a
//! status as two independent comparisons — `HEAD`↔index and index↔worktree —
//! and the same path can appear in both. Collapsing them here, once, is what
//! keeps a caller from inventing a third interpretation: the working tree is
//! the indexed product, and the index is only evidence about how it got there.

use gix::bstr::{BStr, ByteSlice};
use rr_core::cancel::CancelToken;
use rr_core::oid::Oid;
use rr_core::path::RelPath;

use crate::{Error, GitRepo, Result};

/// Where `HEAD` points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadState {
    /// `HEAD` resolves to a commit.
    Commit(Oid),
    /// The repository has no commits yet.
    Unborn,
}

impl HeadState {
    /// The commit this state names, if any.
    #[must_use]
    pub const fn commit(self) -> Option<Oid> {
        match self {
            Self::Commit(oid) => Some(oid),
            Self::Unborn => None,
        }
    }
}

/// What happened to one path, after both status comparisons are collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// The path is new to the index.
    Added,
    /// The path's content or mode differs.
    Modified,
    /// The path no longer has a worktree entry.
    Deleted,
    /// The path moved here from `source`.
    Renamed,
    /// The path was copied from `source`, which is unaffected.
    Copied,
    /// The path is still there but is no longer the same kind of entry.
    TypeChanged,
    /// The path has unmerged index stages.
    Conflicted,
    /// The path exists on disk and Git neither tracks nor ignores it.
    Untracked,
}

/// One observed change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorktreeChange {
    /// What happened.
    pub kind: ChangeKind,
    /// The path it happened to, normalized.
    pub path: RelPath,
    /// Where a rename or copy came from; `None` for every other kind.
    pub source: Option<RelPath>,
}

/// A complete, order-independent observation of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoState {
    /// Where `HEAD` points.
    pub head: HeadState,
    /// Opaque identity of the exact index file that was observed.
    ///
    /// This is not a blob OID and carries no meaning beyond equality: two
    /// observations with the same value saw the same index bytes.
    pub index_checksum: Box<[u8]>,
    /// Every change, sorted and deduplicated.
    pub changes: Vec<WorktreeChange>,
    /// Paths with unmerged index stages, sorted and deduplicated.
    pub conflicted: Vec<RelPath>,
    /// Gitlinks and nested repositories that were excluded rather than entered.
    pub skipped_submodules: u32,
    /// Entries whose stat was racily clean, so Git had to read their content
    /// to decide. Zero on the fast path, by construction.
    pub racy_content_reads: u64,
}

impl RepoState {
    /// Whether anything at all differs from `HEAD`.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty() && self.conflicted.is_empty()
    }
}

/// A change under construction, before its path is known to be addressable.
struct Observation {
    changes: Vec<WorktreeChange>,
    conflicted: Vec<RelPath>,
    skipped_submodules: u32,
}

impl Observation {
    const fn new() -> Self {
        Self {
            changes: Vec::new(),
            conflicted: Vec::new(),
            skipped_submodules: 0,
        }
    }

    /// Records a change, dropping paths that no discovery could ever address.
    ///
    /// Two kinds of path are dropped. One `RelPath` rejects — non-UTF-8, or
    /// otherwise unrepresentable — is not an unknown *status* item: the item
    /// was understood, and its path is simply outside the corpus. `discover`
    /// excludes exactly the same paths, so keeping it here would create a delta
    /// entry no rebuild could ever match.
    ///
    /// The tool's own state directory is dropped for a sharper reason: it is
    /// normally hidden by the ignore rule stamped into it, but a user who
    /// deleted that rule would otherwise make this tool's last output look like
    /// a repository change, and no run could ever again report that nothing
    /// happened. The invariant does not get to depend on a deletable file.
    fn push(&mut self, kind: ChangeKind, path: &BStr, source: Option<&BStr>) {
        let Some(path) = normalize(path) else {
            return;
        };
        if rr_core::workspace::is_private_path(path.as_str()) {
            return;
        }
        if kind == ChangeKind::Conflicted {
            self.conflicted.push(path.clone());
        }
        self.changes.push(WorktreeChange {
            kind,
            path,
            source: source.and_then(normalize),
        });
    }

    fn finish(mut self, head: HeadState, index_checksum: Box<[u8]>, racy: u64) -> RepoState {
        self.changes.sort();
        self.changes.dedup();
        self.conflicted.sort();
        self.conflicted.dedup();
        RepoState {
            head,
            index_checksum,
            changes: self.changes,
            conflicted: self.conflicted,
            skipped_submodules: self.skipped_submodules,
            racy_content_reads: racy,
        }
    }
}

fn normalize(path: &BStr) -> Option<RelPath> {
    RelPath::new(path.to_str().ok()?).ok()
}

/// Whether an index mode names a gitlink rather than a file this index owns.
fn is_gitlink(mode: gix::index::entry::Mode) -> bool {
    mode == gix::index::entry::Mode::COMMIT
}

/// Whether an index mode names a symlink, whose "content" is its target text.
fn is_symlink(mode: gix::index::entry::Mode) -> bool {
    mode == gix::index::entry::Mode::SYMLINK
}

impl GitRepo {
    /// Observes `HEAD`, the index, and the working tree in one pass.
    ///
    /// Untracked files are requested individually and user configuration that
    /// would hide them is deliberately overridden: a caller deciding whether a
    /// rebuild is needed cannot be allowed to inherit someone's `git status`
    /// preferences. Ignored files stay out, because they are not in the corpus.
    ///
    /// Submodules are excluded rather than entered — a dirty nested repository
    /// is that repository's business — and counted so the exclusion is visible.
    ///
    /// The scan runs clean filters of its own: an entry `stat` cannot settle —
    /// a changed mtime, a racy timestamp — sends `gix-status` streaming the
    /// worktree file through the filter to compare hashes, which is most of
    /// them once a filter makes recorded and on-disk sizes disagree in the
    /// first place. A driver that closes stdin early
    /// raises `EPIPE` on that write exactly as [`GitRepo::convert_to_git`]
    /// does, so the same ignore is held here — for the whole iteration, since
    /// the filter runs lazily as items are pulled.
    ///
    /// # Errors
    /// Returns [`Error::Content`] when the status cannot be produced or reports
    /// an item this version does not understand, and [`Error::Cancelled`] when
    /// cancellation was requested mid-scan.
    pub fn observe_state(&self, cancel: &CancelToken) -> Result<RepoState> {
        use gix::status::{Submodule, UntrackedFiles};

        let _sigpipe = crate::sigpipe::ignore();

        let head = match self.head_oid()? {
            Some(oid) => HeadState::Commit(oid),
            None => HeadState::Unborn,
        };
        let index_checksum = self.index_checksum()?;

        let platform = self
            .gix_repo()
            .status(gix::progress::Discard)
            .map_err(|error| Error::Content(format!("git status unavailable: {error}")))?
            .untracked_files(UntrackedFiles::Files)
            .index_worktree_submodules(Submodule::Given {
                ignore: gix::submodule::config::Ignore::All,
                check_dirty: false,
            })
            .should_interrupt_owned(cancel.flag());

        let mut iter = platform.into_iter(None).map_err(|error| {
            if cancel.is_cancelled() {
                Error::Cancelled
            } else {
                Error::Content(format!("git status failed to start: {error}"))
            }
        })?;

        let mut observation = Observation::new();
        for item in iter.by_ref() {
            let item = item.map_err(|error| {
                if cancel.is_cancelled() {
                    Error::Cancelled
                } else {
                    Error::Content(format!("git status item failed: {error}"))
                }
            })?;
            match item {
                gix::status::Item::TreeIndex(change) => observation.tree_index(&change),
                gix::status::Item::IndexWorktree(change) => observation.index_worktree(&change),
            }
        }

        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let racy = iter.outcome_mut().map_or(0, |outcome| {
            u64::try_from(outcome.index_worktree.tracked_file_modification.racy_clean)
                .unwrap_or(u64::MAX)
        });

        Ok(observation.finish(head, index_checksum, racy))
    }

    /// Hashes the index file as an opaque identity.
    ///
    /// The index's own trailing checksum would be cheaper, but `index.skipHash`
    /// makes it optional, and an identity that is sometimes absent is not an
    /// identity. An absent index file hashes as the empty input, which is
    /// exactly what a repository with no index has.
    fn index_checksum(&self) -> Result<Box<[u8]>> {
        let path = self.gix_repo().index_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(Error::Io(error)),
        };
        Ok(Box::from(blake3::hash(&bytes).as_bytes().as_slice()))
    }
}

impl Observation {
    /// Maps one `HEAD`↔index difference.
    fn tree_index(&mut self, change: &gix::diff::index::Change) {
        use gix::diff::index::Change;

        match change {
            Change::Addition {
                location,
                entry_mode,
                ..
            } => {
                if is_gitlink(*entry_mode) {
                    self.skipped_submodules += 1;
                } else {
                    self.push(ChangeKind::Added, location.as_ref(), None);
                }
            }
            Change::Deletion {
                location,
                entry_mode,
                ..
            } => {
                if is_gitlink(*entry_mode) {
                    self.skipped_submodules += 1;
                } else {
                    self.push(ChangeKind::Deleted, location.as_ref(), None);
                }
            }
            Change::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                if is_gitlink(*entry_mode) || is_gitlink(*previous_entry_mode) {
                    self.skipped_submodules += 1;
                } else if is_symlink(*previous_entry_mode) != is_symlink(*entry_mode) {
                    self.push(ChangeKind::TypeChanged, location.as_ref(), None);
                } else {
                    self.push(ChangeKind::Modified, location.as_ref(), None);
                }
            }
            Change::Rewrite {
                source_location,
                location,
                entry_mode,
                copy,
                ..
            } => {
                if is_gitlink(*entry_mode) {
                    self.skipped_submodules += 1;
                } else {
                    let kind = if *copy {
                        ChangeKind::Copied
                    } else {
                        ChangeKind::Renamed
                    };
                    self.push(kind, location.as_ref(), Some(source_location.as_ref()));
                }
            }
        }
    }

    /// Maps one index↔worktree difference, including dirwalk findings.
    fn index_worktree(&mut self, item: &gix::status::index_worktree::Item) {
        use gix::status::index_worktree::Item;

        match item {
            Item::Modification {
                entry,
                rela_path,
                status,
                ..
            } => self.entry_status(entry, rela_path.as_ref(), status),
            Item::DirectoryContents { entry, .. } => self.dirwalk_entry(entry),
            Item::Rewrite {
                source,
                dirwalk_entry,
                copy,
                ..
            } => {
                let kind = if *copy {
                    ChangeKind::Copied
                } else {
                    ChangeKind::Renamed
                };
                self.push(
                    kind,
                    dirwalk_entry.rela_path.as_ref(),
                    Some(source.rela_path()),
                );
            }
        }
    }

    /// Maps one tracked entry's worktree state.
    fn entry_status(
        &mut self,
        entry: &gix::index::Entry,
        rela_path: &BStr,
        status: &gix::status::plumbing::index_as_worktree::EntryStatus<(), gix::submodule::Status>,
    ) {
        use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

        if is_gitlink(entry.mode) {
            self.skipped_submodules += 1;
            return;
        }

        match status {
            EntryStatus::Conflict { .. } => self.push(ChangeKind::Conflicted, rela_path, None),

            EntryStatus::IntentToAdd => self.push(ChangeKind::Added, rela_path, None),

            EntryStatus::NeedsUpdate(_) => {}
            EntryStatus::Change(Change::Removed) => self.push(ChangeKind::Deleted, rela_path, None),
            EntryStatus::Change(Change::Type { .. }) => {
                self.push(ChangeKind::TypeChanged, rela_path, None);
            }
            EntryStatus::Change(Change::Modification { .. }) => {
                self.push(ChangeKind::Modified, rela_path, None);
            }
            EntryStatus::Change(Change::SubmoduleModification(_)) => {
                self.skipped_submodules += 1;
            }
        }
    }

    /// Maps one directory-walk finding.
    fn dirwalk_entry(&mut self, entry: &gix::dir::Entry) {
        use gix::dir::entry::{Kind, Status};

        match entry.status {

            Status::Tracked | Status::Ignored(_) | Status::Pruned => return,
            Status::Untracked => {}
        }

        match entry.disk_kind {

            Some(Kind::Repository) => self.skipped_submodules += 1,
            Some(Kind::File | Kind::Symlink) => {
                self.push(ChangeKind::Untracked, entry.rela_path.as_ref(), None);
            }

            Some(Kind::Directory | Kind::Untrackable) | None => {}
        }
    }
}

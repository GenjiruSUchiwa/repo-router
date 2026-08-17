//! Deciding what a run must do, separately from doing it.
//!
//! `rr refresh` and `rr status` ask the same question — *how much of this
//! snapshot is still true?* — and differ only in whether they act on the answer.
//! Asking it in one place is not tidiness: a status that computed freshness
//! independently would eventually disagree with the refresh it is describing,
//! and the disagreement would show up as a user reporting that `rr status` says
//! `fresh` while `rr refresh` rebuilds every time.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rr_core::cancel::CancelToken;
use rr_core::index::{ContentRepresentation, Snapshot};
use rr_core::path::RelPath;
use rr_core::refresh::{FullReason, PlanDraft, RefreshMode, RefreshPlan};
use rr_core::snapshot::{LoadOutcome, RebuildReason, SnapshotStore};
use rr_core::Oid;

use rr_core::walk::{collected_lang, WalkCfg};

use crate::content::ContentProbe;
use crate::map::{is_regular_file, BuildContext};
use crate::repo::{ChangeKind, CommittedChange, CommittedDelta, CommittedKind, RepoState};
use crate::{GitRepo, Result};

/// The snapshot currently on disk, or the reason it cannot be built upon.
pub enum Published {
    /// A snapshot this binary understands and has fully validated.
    Ready(Arc<Snapshot>),
    /// There is nothing to build upon, and this is why.
    Unusable(FullReason),
}

impl Published {
    /// Loads and strictly validates the published snapshot.
    ///
    /// # Errors
    /// Returns an error only for I/O failures. A snapshot that is absent,
    /// unreadable by this version, or fails validation is not an error — it is
    /// the ordinary reason a rebuild happens, and it is reported as one.
    pub fn load(store: &SnapshotStore) -> Result<Self> {
        Ok(match store.load()? {
            LoadOutcome::Ready(snapshot) => Self::Ready(snapshot),
            LoadOutcome::Missing => Self::Unusable(FullReason::MissingSnapshot),
            LoadOutcome::NeedsRebuild(reason) => Self::Unusable(rebuild_reason(&reason)),
        })
    }

    /// The snapshot, when there is a usable one.
    #[must_use]
    pub const fn snapshot(&self) -> Option<&Arc<Snapshot>> {
        match self {
            Self::Ready(snapshot) => Some(snapshot),
            Self::Unusable(_) => None,
        }
    }
}

/// Maps a load failure onto the reason a full rebuild is happening.
///
/// The distinction the caller cares about is whether the file was *wrong* or
/// merely *older*: a version mismatch is expected after an upgrade and needs no
/// explanation, while a checksum failure means something damaged the file and a
/// user might want to know.
const fn rebuild_reason(reason: &RebuildReason) -> FullReason {
    match *reason {
        RebuildReason::UnsupportedVersion { .. }
        | RebuildReason::LexicalMismatch
        | RebuildReason::BuildVersionMismatch { .. }
        | RebuildReason::RankingProfileMismatch { .. } => FullReason::IncompatibleSnapshot,
        RebuildReason::BadMagic
        | RebuildReason::LengthMismatch
        | RebuildReason::ChecksumMismatch
        | RebuildReason::InvalidPayload
        | RebuildReason::TrailingBytes
        | RebuildReason::InvalidInvariant => FullReason::CorruptSnapshot,
    }
}

/// Observes the repository, or reports that there is none to observe.
///
/// A root outside Git has no delta to consult and no cheap way to learn what
/// changed, so it always rebuilds. That is a property of the root, not a
/// failure, which is why it produces `None` rather than an error.
///
/// # Errors
/// Returns observation failures, including [`crate::Error::Cancelled`].
pub fn observe(context: &BuildContext, cancel: &CancelToken) -> Result<Option<RepoState>> {
    match context.repo()? {
        Some(repo) => repo.observe_state(cancel).map(Some),
        None => Ok(None),
    }
}

/// A plan and what deciding on it cost.
///
/// Deciding is not free any more: proving a dirty file unchanged means reading
/// it. A read that no counter sees is exactly the kind of cost that turns a
/// benchmark regression into a mystery, so it is reported rather than absorbed.
pub struct Planned {
    /// What this run is allowed to skip.
    pub plan: RefreshPlan,
    /// Content acquisitions the planner performed to reach that conclusion.
    pub content_reads: u64,
}

impl Planned {
    /// A plan that cost nothing to reach.
    fn free(plan: RefreshPlan) -> Self {
        Self {
            plan,
            content_reads: 0,
        }
    }
}

/// Decides what this run is allowed to skip.
#[must_use]
pub fn plan_for(
    mode: RefreshMode,
    published: &Published,
    observed: Option<&RepoState>,
    digest: [u8; 32],
    repo: Option<&GitRepo>,
    walk: &WalkCfg,
) -> Planned {
    if mode == RefreshMode::Full {
        return Planned::free(RefreshPlan::requested_full());
    }

    let snapshot = match published {
        Published::Ready(snapshot) => snapshot,
        Published::Unusable(reason) => return Planned::free(RefreshPlan::full(*reason)),
    };
    let Some(observed) = observed else {
        return Planned::free(RefreshPlan::full(FullReason::GitStatusUnavailable));
    };

    let committed = match (snapshot.meta.repo_head_oid, observed.head.commit()) {
        (Some(before), Some(after)) if before != after => {
            let delta = repo.map_or(CommittedDelta::Rebuild(FullReason::HeadChanged), |repo| {
                repo.committed_delta(before, after)
            });
            match delta {
                CommittedDelta::Paths(paths) => paths,
                CommittedDelta::Rebuild(reason) => return Planned::free(RefreshPlan::full(reason)),
            }
        }
        (before, after) if before == after => Vec::new(),
        _ => return Planned::free(RefreshPlan::full(FullReason::HeadChanged)),
    };

    if snapshot.meta.discovery_digest != digest {
        return Planned::free(RefreshPlan::full(FullReason::DiscoveryRulesChanged));
    }

    draft_from(observed, snapshot, repo, walk, &committed)
}

/// Whether the commits between the snapshot's `HEAD` and `after` left this
/// index true, asked without looking at the working tree.
///
/// [`plan_for`] answers a strictly larger question and needs [`observe`] to do
/// it: a walk of the whole tree, which is the right price for `rr status` and
/// the wrong one for `rr query`. This asks only the part a moved `HEAD` raises
/// — *did the commits in between touch anything this index is a statement
/// about?* — and it asks it out of the commit's own tree diff, so the cost is
/// proportional to what was committed rather than to what exists.
///
/// The predicates are [`plan_for`]'s, reached through the same [`Evidence`],
/// which is what keeps the two from drifting into different answers about one
/// repository. Rule paths are the one addition: discovery is a function of them
/// and this has no observation to recompute the digest from, so a commit that
/// touches one is refused rather than reasoned about.
///
/// Every uncertainty answers `false` — no `HEAD` on either side, no repository,
/// a diff the object store would not produce. That is a slow answer (`rr
/// refresh`) and never a wrong one.
///
/// What it does **not** see is a working-tree edit, and deliberately: neither
/// does the caller's equal-ids branch, which never asks anything at all. So this
/// grants a moved `HEAD` exactly the trust an unmoved one already had, and no
/// more. Observing the tree is `rr status`'s job, and it is still the only
/// command that does it.
///
/// # Errors
/// Returns discovery and repository-open failures.
pub fn commits_left_index_true(
    root: &Path,
    snapshot: &Snapshot,
    after: Option<Oid>,
) -> Result<bool> {
    let (Some(before), Some(after)) = (snapshot.meta.repo_head_oid, after) else {
        return Ok(false);
    };
    if before == after {
        return Ok(true);
    }

    let context = BuildContext::open(root, 1)?;
    let Some(repo) = context.repo()? else {
        return Ok(false);
    };
    let CommittedDelta::Paths(committed) = repo.committed_delta(before, after) else {
        return Ok(false);
    };

    let mut draft = PlanDraft::new();
    let mut evidence = Evidence {
        snapshot,
        repo: Some(&repo),
        walk: &context.walk,
        content_reads: 0,
    };
    for change in &committed {
        if crate::rules::is_rule_path(&change.path) {
            return Ok(false);
        }
        match change.kind {
            CommittedKind::Removed => evidence.remove(&mut draft, &change.path),
            CommittedKind::Touched => evidence.recheck(&mut draft, &change.path),
        }
    }

    Ok(draft.build().is_ok_and(|plan| plan.is_empty_delta()))
}

/// Turns observed changes into a delta, or into the reason there cannot be one.
///
/// Two things are reconsidered beyond the obvious. Paths the snapshot recorded
/// from a dirty working tree are rechecked even when the current delta says
/// nothing about them, because "no longer differs from `HEAD`" is a change the
/// snapshot needs to hear about and Git status has no way to say it. And every
/// candidate is measured against what the snapshot already recorded, because a
/// delta is a statement about `HEAD` while a snapshot is a statement about the
/// working tree — a file edited before the last refresh differs from `HEAD`
/// forever, and rechecking it forever would mean `rr status` reporting `stale`
/// on any tree with uncommitted work no matter how many refreshes ran.
fn draft_from(
    observed: &RepoState,
    snapshot: &Snapshot,
    repo: Option<&GitRepo>,
    walk: &WalkCfg,
    committed: &[CommittedChange],
) -> Planned {
    let mut draft = PlanDraft::new();
    let mut evidence = Evidence {
        snapshot,
        repo,
        walk,
        content_reads: 0,
    };

    let now_dirty: BTreeSet<&RelPath> = observed
        .changes
        .iter()
        .flat_map(|change| std::iter::once(&change.path).chain(change.source.as_ref()))
        .collect();
    let mut spoken_for: BTreeSet<&RelPath> = now_dirty.clone();
    for path in &snapshot.meta.dirty_paths {
        if spoken_for.insert(path) {
            evidence.recheck(&mut draft, path);
        }
    }

    for change in committed {
        if !spoken_for.insert(&change.path) {
            continue;
        }
        match change.kind {
            CommittedKind::Removed => evidence.remove(&mut draft, &change.path),
            CommittedKind::Touched => evidence.recheck(&mut draft, &change.path),
        }
    }

    for change in &observed.changes {
        match change.kind {
            ChangeKind::Deleted => evidence.remove(&mut draft, &change.path),

            ChangeKind::Conflicted => evidence.conflict(&mut draft, &change.path),
            ChangeKind::Renamed => match change.source.clone() {
                Some(source) => evidence.rename(&mut draft, source, &change.path),

                None => draft.recheck(change.path.clone()),
            },

            ChangeKind::Copied
            | ChangeKind::Added
            | ChangeKind::Modified
            | ChangeKind::TypeChanged
            | ChangeKind::Untracked => evidence.recheck(&mut draft, &change.path),
        }
    }

    let plan = draft
        .build()
        .unwrap_or_else(|_| RefreshPlan::full(FullReason::ContradictoryDelta));
    evidence.into_planned(plan)
}

/// What a delta is drafted against, and what consulting it has cost so far.
///
/// The snapshot and the repository travel together because neither answers the
/// question alone: one holds what was recorded, the other what is there now.
struct Evidence<'a> {
    snapshot: &'a Snapshot,
    repo: Option<&'a GitRepo>,
    walk: &'a WalkCfg,
    content_reads: u64,
}

impl Evidence<'_> {
    /// Reconsiders `path` unless there is evidence that nothing would come of it.
    fn recheck(&mut self, draft: &mut PlanDraft, path: &RelPath) {
        if self.declined(path) || self.already_recorded(path) {
            return;
        }
        draft.recheck(path.clone());
    }

    /// Flags a conflict at `path`, unless this index has no stake in the file.
    fn conflict(&self, draft: &mut PlanDraft, path: &RelPath) {
        if !self.declined(path) {
            draft.conflict(path.clone());
        }
    }

    /// Drops `path` unless the snapshot has already dropped it.
    ///
    /// An uncommitted deletion is reported by every status until it is
    /// committed. Removing a record that is no longer there would keep the
    /// delta non-empty for ever, which is the same trap [`Self::recheck`]
    /// avoids, reached from the other direction.
    fn remove(&mut self, draft: &mut PlanDraft, path: &RelPath) {
        if self.holds(path) {
            draft.remove(path.clone());
        }
    }

    /// Moves a record from `source` to `target`, unless it has already moved.
    ///
    /// The pair is kept intact rather than decomposed so that the draft still
    /// sees a rename and can reject one path claiming two targets.
    fn rename(&mut self, draft: &mut PlanDraft, source: RelPath, target: &RelPath) {
        let target_matters = !self.declined(target) && !self.already_recorded(target);
        if self.holds(&source) || target_matters {
            draft.rename(source, target.clone());
        }
    }

    /// Whether the snapshot has a record at `path` at all.
    fn holds(&self, path: &RelPath) -> bool {
        self.snapshot.file_by_path(path.as_str()).is_some()
    }

    /// Whether discovery is known not to collect this path.
    ///
    /// Git reports a change to any file it tracks, and most files are not this
    /// index's business: a `README` edit, a lock file, a source file inside an
    /// ignored directory. Each of those differs from `HEAD` until it is
    /// committed, so a delta that carried them would never empty — `rr status`
    /// would answer `stale` for as long as the edit lived, and the refresh it
    /// demanded would have nothing to do.
    ///
    /// Three things settle it, cheapest first, and each is conclusive rather
    /// than probable. The name may rule the path out under this configuration.
    /// The previous build may have walked the tree with the file in it and
    /// produced no record. Or there may be no file there at all — a symlink, a
    /// path that has since gone — which the previous build cannot answer for
    /// and only looking can. Anything else is not evidence, and the path is
    /// reconsidered.
    ///
    /// A path the snapshot already holds is never declined on any of it except
    /// the name. What becomes of that record is precisely what this run has to
    /// decide, and the other two answers are not stable enough to decide it: a
    /// file that is missing at this instant may be back by the time the walk
    /// reaches it, and retaining a record against content nobody re-examined is
    /// how a stale fact gets published.
    fn declined(&self, path: &RelPath) -> bool {
        if collected_lang(path.as_str(), self.walk).is_none() {
            return true;
        }
        if self.holds(path) {
            return false;
        }
        if self.snapshot.meta.skipped_paths.binary_search(path).is_ok() {
            return true;
        }
        self.repo
            .is_some_and(|repo| !is_regular_file(repo.workdir(), path))
    }

    fn into_planned(self, plan: RefreshPlan) -> Planned {
        Planned {
            plan,
            content_reads: self.content_reads,
        }
    }

    /// Whether the snapshot already records exactly the bytes now at `path`.
    ///
    /// This is the same identity the pipeline would compute, asked before the
    /// work instead of after it, so a path it clears is one retention handles
    /// correctly: the recorded oid names the recorded facts, and the language
    /// and generated flags are functions of the path, which has not moved.
    ///
    /// Every uncertainty answers "no". A file that vanished, an object store
    /// that will not read, a path the snapshot never held — each means the
    /// evidence for skipping is absent, so the path is reconsidered. That costs
    /// work and cannot cost correctness, and the failure resurfaces in the
    /// pipeline where it has somewhere to go.
    fn already_recorded(&mut self, path: &RelPath) -> bool {

        if self.snapshot.meta.dirty_paths.binary_search(path).is_err() {
            return false;
        }
        let (Some(repo), Some(record)) = (self.repo, self.snapshot.file_by_path(path.as_str()))
        else {
            return false;
        };
        let Ok(probe) = repo.probe_content(path) else {
            return false;
        };

        match probe {

            ContentProbe::CleanGitBlob(oid) => {
                oid == record.content_oid
                    && record.representation == ContentRepresentation::GitCanonical
            }

            ContentProbe::ReadRequired => {
                self.content_reads += 1;
                repo.acquire_content(path, ContentProbe::ReadRequired)
                    .ok()
                    .flatten()
                    .is_some_and(|content| {
                        content.oid == record.content_oid
                            && content.representation == record.representation
                    })
            }
        }
    }
}

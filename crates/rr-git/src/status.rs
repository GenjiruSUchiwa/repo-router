//! Answering "is the snapshot still true?" without changing anything.
//!
//! Status is a refresh that stops after deciding. It runs the same load, the
//! same observation and the same planner, and then reports the plan instead of
//! executing it — so `fresh` means precisely "a refresh right now would take the
//! no-op path", by construction rather than by agreement.
//!
//! Nothing here writes, creates a directory, or takes the publication lock.
//! A read-only command that leaves a `.rr` behind in a repository the user only
//! asked a question about would be a surprise, and one that blocked a concurrent
//! refresh would be worse.

use std::path::Path;

use rr_core::cancel::CancelToken;
use rr_core::refresh::{
    FullReason, GitLabel, RefreshMode, RefreshPlan, SnapshotLabel, StatusReport,
};
use rr_core::snapshot::SnapshotStore;
use rr_core::Oid;

use crate::map::BuildContext;
use crate::plan::{observe, plan_for, Published};
use crate::repo::RepoState;
use crate::rules::discovery_digest;
use crate::{Error, Result};

/// Reports how the repository at `root` and its published snapshot relate.
///
/// # Errors
/// Returns [`Error::Cancelled`] when the caller asked to stop, and I/O or
/// discovery failures. A repository whose working tree cannot be compared is
/// not an error — it is reported as [`GitLabel::Unavailable`].
pub fn status(root: &Path, cancel: &CancelToken) -> Result<StatusReport> {
    // One thread: status observes and reads, and never runs the parser pool.
    let context = BuildContext::open(root, 1)?;
    let store = SnapshotStore::new(&context.work_root);
    let published = Published::load(&store)?;

    // Read from the snapshot regardless of what the repository turns out to
    // say: this describes the file queries will actually use, and it stays true
    // whether that file is fresh, stale, or merely unverifiable.
    let unresolved = published
        .snapshot()
        .map_or(0, |snapshot| snapshot.unresolved_count());

    let observed = match observe(&context, cancel) {
        Ok(observed) => observed,
        // An interrupted scan has observed nothing, and "nothing observed" is
        // indistinguishable from "nothing to observe". Reporting a cancelled
        // run as a clean tree is the failure this guard exists to prevent.
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(_) => {
            let (git, head, snapshot) = unobservable(&context, &published)?;
            return Ok(StatusReport {
                git,
                head,
                snapshot,
                stale_paths: None,
                unresolved,
            });
        }
    };

    let repo = context.repo()?;
    let digest = discovery_digest(repo.as_ref(), &context.walk, observed.as_ref());
    // Status reports no counters, so what deciding cost is dropped here rather
    // than reported. It is the same cost the refresh it describes would pay.
    let plan = plan_for(
        RefreshMode::Incremental,
        &published,
        observed.as_ref(),
        digest,
        repo.as_ref(),
        &context.walk,
    )
    .plan;

    Ok(StatusReport {
        git: observed.as_ref().map_or(GitLabel::NoGit, git_label),
        head: observed.as_ref().and_then(|state| state.head.commit()),
        snapshot: snapshot_label(&plan),
        stale_paths: stale_paths(&plan),
        unresolved,
    })
}

/// What can still be said about a repository whose tree cannot be compared.
///
/// `HEAD` is still asked for: it is read from the ref store rather than from
/// the working tree, so whatever made the tree uncomparable usually has no
/// bearing on it, and a commit id is the most useful single fact left. If even
/// that fails, the repository is broken in a way worth failing over.
fn unobservable(
    context: &BuildContext,
    published: &Published,
) -> Result<(GitLabel, Option<Oid>, SnapshotLabel)> {
    let head = context.repo()?.map(|repo| repo.head_oid()).transpose()?;

    // There is a snapshot and no way to tell whether it still holds. Saying
    // `stale` would be a claim with no evidence for it, and `fresh` a claim
    // with no evidence against it.
    let snapshot = match published {
        Published::Ready(_) => SnapshotLabel::Unknown,
        Published::Unusable(reason) => unusable_label(*reason),
    };
    Ok((GitLabel::Unavailable, head.flatten(), snapshot))
}

/// How the working tree relates to the index and `HEAD`.
///
/// Conflict outranks dirt: an unmerged path is the fact the user has to act on,
/// and it would be buried by the modifications that always accompany it.
fn git_label(state: &RepoState) -> GitLabel {
    if !state.conflicted.is_empty() {
        GitLabel::Conflicted
    } else if state.is_clean() {
        GitLabel::Clean
    } else {
        GitLabel::Dirty
    }
}

/// How the published snapshot relates to the working tree.
///
/// `Fresh` is the same predicate the refresh no-op path uses, asked of the same
/// plan — the label cannot drift from the behaviour it describes.
fn snapshot_label(plan: &RefreshPlan) -> SnapshotLabel {
    match (plan.mode(), plan.reason()) {
        (RefreshMode::Incremental, _) if plan.is_empty_delta() => SnapshotLabel::Fresh,
        (RefreshMode::Incremental, _) => SnapshotLabel::Stale,
        (RefreshMode::Full, Some(reason)) => unusable_label(reason),
        // A full rebuild nobody had to justify is one that was asked for
        // outright, and asking for it says nothing about the snapshot. Status
        // never requests one, so this is a guard rather than a path.
        (RefreshMode::Full, None) => SnapshotLabel::Unknown,
    }
}

/// The label for a rebuild the planner has already justified.
///
/// The distinction that matters is *observation* versus *ignorance*.
/// `GitStatusUnavailable` is the only reason that reports no fact about the
/// snapshot at all: calling it `stale` would tell every user outside Git that
/// their snapshot is permanently out of date no matter what they do. A
/// contradictory delta is on the other side of that line — the repository was
/// observed, and a rebuild will follow from what it said.
const fn unusable_label(reason: FullReason) -> SnapshotLabel {
    match reason {
        FullReason::MissingSnapshot => SnapshotLabel::Missing,
        FullReason::IncompatibleSnapshot => SnapshotLabel::Incompatible,
        FullReason::CorruptSnapshot => SnapshotLabel::Corrupt,
        FullReason::HeadChanged
        | FullReason::DiscoveryRulesChanged
        | FullReason::ContradictoryDelta => SnapshotLabel::Stale,
        FullReason::GitStatusUnavailable => SnapshotLabel::Unknown,
    }
}

/// How many paths a refresh would reconsider, when that is knowable.
///
/// A plan that fell back to a full rebuild reconsiders everything, and
/// "everything" is a number no one has counted — it costs the walk the fast
/// path exists to avoid. `None` says so rather than guessing.
fn stale_paths(plan: &RefreshPlan) -> Option<u64> {
    (plan.mode() == RefreshMode::Incremental)
        .then(|| u64::try_from(plan.touched_len()).unwrap_or(u64::MAX))
}

//! Bringing the published snapshot back into agreement with the repository.
//!
//! The cheap path exists because Git already knows what changed. The expensive
//! path exists because Git's answer is only usable when several other things
//! held still: the same commit, the same index, the same discovery rules, and a
//! snapshot that was built under the same versions. Every reason the delta
//! cannot be trusted is named in [`FullReason`] and ends the same way — a full
//! rebuild — so a wrong answer is never among the outcomes.
//!
//! The order of operations matters as much as the operations. Nothing is
//! locked, created, or written until the state has been observed and found
//! wanting, so the overwhelmingly common case of "nothing happened" costs one
//! snapshot read and one status scan.

use std::path::Path;
use std::time::Instant;

use rr_core::cancel::CancelToken;
use rr_core::index::{ContentRepresentation, Snapshot};
use rr_core::lang::Lang;
use rr_core::path::RelPath;
use rr_core::refresh::{RefreshMode, RefreshOutcome, RefreshPlan, RefreshReport, ReportedMode};
use rr_core::snapshot::SnapshotStore;
use rr_core::walk::discover;
use rr_core::{FactCache, Oid};

use crate::content::ContentProbe;
use crate::guard::RepositoryWriteGuard;
use crate::map::{BuildContext, BuiltInputs};
use crate::plan::{observe, plan_for, Published};
use crate::repo::RepoState;
use crate::rules::discovery_digest;
use crate::{Error, GitRepo, Result};

/// Refreshes the snapshot for the repository rooted at `root`.
///
/// # Errors
/// Returns [`Error::PublicationLocked`] when another process is publishing,
/// [`Error::Cancelled`] when the caller asked to stop, [`Error::RepositoryChanged`]
/// when the repository moved while the snapshot was being built, and acquisition,
/// extraction, or I/O failures otherwise.
pub fn refresh(
    root: &Path,
    threads: usize,
    mode: RefreshMode,
    cancel: &CancelToken,
) -> Result<RefreshReport> {
    let started = Instant::now();
    let mut report = RefreshReport::default();

    let context = BuildContext::open(root, threads)?;
    let store = SnapshotStore::new(&context.work_root);
    // One handle for both planning passes: planning probes content identity for
    // the delta, and a handle that has already warmed its object cache is the
    // one that makes the second pass nearly free.
    let repo = context.repo()?;

    // Everything needed to *decide* is gathered before anything is claimed or
    // created: a run with no work to do must leave no trace that it ran.
    let published = Published::load(&store)?;
    let observed = observe(&context, cancel)?;
    let digest = discovery_digest(repo.as_ref(), &context.walk, observed.as_ref());
    let planned = plan_for(
        mode,
        &published,
        observed.as_ref(),
        digest,
        repo.as_ref(),
        &context.walk,
    );
    report.content_reads = planned.content_reads;

    if is_no_op(&store, &planned.plan)? {
        record_plan(&mut report, &planned.plan);
        report.elapsed_ms = elapsed_ms(started);
        return Ok(report);
    }

    let _guard = RepositoryWriteGuard::acquire(&context.work_root)?;

    // The repository is observed again under the lock. Between the first
    // observation and the claim, anything at all could have happened; planning
    // from the earlier reading would build a snapshot describing a repository
    // that no longer exists.
    let observed = observe(&context, cancel)?;
    let digest = discovery_digest(repo.as_ref(), &context.walk, observed.as_ref());
    let planned = plan_for(
        mode,
        &published,
        observed.as_ref(),
        digest,
        repo.as_ref(),
        &context.walk,
    );
    let plan = planned.plan;
    // Both planning passes read, and both reads happened. A counter that
    // reported only the second would understate a cost the caller paid.
    report.content_reads += planned.content_reads;
    record_plan(&mut report, &plan);

    let built = build(&context, &plan, &published, cancel)?;
    report.reparsed = built.stats().parses;
    report.cached = built.stats().cache_hits;
    report.cache_corrupt = built.stats().cache_corrupt;
    report.degraded = built.stats().degraded;
    report.content_reads += built.stats().clean_blob_reads + built.stats().filtered_raw_reads;

    // Derived before the inputs are consumed by assembly: this is the list of
    // claims the run has to stand behind.
    let read = acquired(&published, &plan, &built);

    let outcome = context.assemble(built, observed.as_ref())?;
    let envelope = store.encode(&outcome.snapshot)?;

    // Validation happens after the bytes exist and before they are published,
    // because the question is not "was the repository stable while we read it"
    // but "is this snapshot true right now".
    confirm_unchanged(&context, observed.as_ref(), digest, cancel)?;
    confirm_content(repo.as_ref(), &read, &mut report.content_reads)?;
    check_cancelled(cancel)?;

    report.snapshot_updated = store.publish(&envelope)?;
    report.outcome = if report.snapshot_updated {
        RefreshOutcome::Updated
    } else {
        RefreshOutcome::Unchanged
    };
    report.elapsed_ms = elapsed_ms(started);
    Ok(report)
}

/// Reports "nothing to do" without locking, building, or writing anything.
///
/// Returns `None` when there is work, so the caller continues. The fast path
/// is deliberately narrow: an incremental plan, an empty delta, and a snapshot
/// whose own bytes are already on disk.
fn is_no_op(store: &SnapshotStore, plan: &RefreshPlan) -> Result<bool> {
    if plan.mode() != RefreshMode::Incremental || !plan.is_empty_delta() {
        return Ok(false);
    }

    // The published file has to still be there. Checking costs one read, and
    // the alternative is reporting success for a snapshot someone deleted —
    // leaving the repository with no index and this run claiming it has one.
    Ok(store.read_published()?.is_some())
}

/// Copies the plan's shape into the report.
fn record_plan(report: &mut RefreshReport, plan: &RefreshPlan) {
    report.mode = match (plan.mode(), plan.reason()) {
        (RefreshMode::Incremental, _) => ReportedMode::Incremental,
        (RefreshMode::Full, None) => ReportedMode::Full,
        (RefreshMode::Full, Some(_)) => ReportedMode::FallbackFull,
    };
    report.fallback_reason = plan.reason();
    report.changed = count(plan.recheck().len());
    report.removed = count(plan.remove().len());
    report.renamed = count(plan.renames().len());
    report.conflicted = count(plan.conflicted().len());
}

/// Processes every discovered file, retaining what the plan permits.
fn build(
    context: &BuildContext,
    plan: &RefreshPlan,
    published: &Published,
    cancel: &CancelToken,
) -> Result<BuiltInputs> {
    check_cancelled(cancel)?;
    let files = discover(&context.work_root, &context.walk)?;
    let cache = FactCache::open(&context.work_root)?;
    check_cancelled(cancel)?;

    // A full rebuild retains nothing, which is the whole of the difference
    // between the two modes: same files, same pipeline, no shortcut.
    let retainable = published
        .snapshot()
        .filter(|_| plan.mode() == RefreshMode::Incremental);

    context.run(&files, |worker, source| {
        // Cancellation is checked per file rather than per batch, because a
        // repository large enough to be worth interrupting is one where a
        // batch takes long enough that the interruption would not be felt.
        check_cancelled(cancel)?;

        match retainable.and_then(|snapshot| {
            retained(snapshot, plan, &source.path, source.lang, source.generated)
        }) {
            Some((oid, representation)) => worker.retain(source, oid, representation, &cache),
            None => worker.process(source, &cache),
        }
    })
}

/// What the previous snapshot already established about a path, when the plan
/// says this run is allowed to believe it.
///
/// Takes the three things retention actually depends on rather than a whole
/// [`SourceFile`], so the same decision can be re-asked afterwards of the
/// inputs the build produced. Asking it twice through one function is what
/// keeps "what was verified" equal to "what was read".
fn retained(
    snapshot: &Snapshot,
    plan: &RefreshPlan,
    path: &RelPath,
    language: Lang,
    generated: bool,
) -> Option<(Oid, ContentRepresentation)> {
    if !plan.may_retain(path) {
        return None;
    }
    let record = snapshot.file_by_path(path.as_str())?;
    // A file whose language or generated classification changed is a different
    // file as far as the index is concerned, even at identical content — and
    // neither of those is something Git status would ever report.
    if record.language != language || record.generated != generated {
        return None;
    }
    Some((record.content_oid, record.representation))
}

/// The paths whose bytes this run acquired, and the identity it acquired them
/// under.
///
/// Everything the plan did not permit retaining was read, and every read is a
/// claim about bytes that could have moved since. The set is derived by asking
/// [`retained`] the same question the build asked, of the inputs the build
/// produced, so a change to the retention rule cannot leave a read unverified.
fn acquired(
    published: &Published,
    plan: &RefreshPlan,
    built: &BuiltInputs,
) -> Vec<(RelPath, Oid, ContentRepresentation)> {
    let retainable = published
        .snapshot()
        .filter(|_| plan.mode() == RefreshMode::Incremental);

    built
        .inputs()
        .filter(|input| {
            retainable
                .and_then(|snapshot| {
                    retained(snapshot, plan, &input.path, input.language, input.generated)
                })
                .is_none()
        })
        .map(|input| (input.path.clone(), input.oid, input.representation))
        .collect()
}

/// Re-acquires every path this run read and confirms it still reads the same.
///
/// A matching observation proves the repository still *says* the same thing,
/// which is a weaker claim than it looks: a file can be rewritten while already
/// modified without changing one word of its status. Only the bytes settle it,
/// and the bytes are what the snapshot is about.
///
/// The cost is bounded by what was read rather than by the repository: a path
/// Git can certify from its index stat needs no read to re-confirm, so a clean
/// tree verifies for free and a dirty one pays once per changed file.
fn confirm_content(
    repo: Option<&GitRepo>,
    acquired: &[(RelPath, Oid, ContentRepresentation)],
    reads: &mut u64,
) -> Result<()> {
    let Some(repo) = repo else {
        // Outside Git every run is a full rebuild from the bytes just read, so
        // there is no earlier claim left to re-confirm.
        return Ok(());
    };

    for (path, oid, representation) in acquired {
        let probe = repo.probe_content(path)?;
        if probe == ContentProbe::ReadRequired {
            *reads += 1;
        }
        let content = repo
            .acquire_content(path, probe)?
            .ok_or(Error::RepositoryChanged)?;
        if content.oid != *oid || content.representation != *representation {
            return Err(Error::RepositoryChanged);
        }
    }
    Ok(())
}

/// Re-checks that the repository still says what it said when the plan was made.
///
/// The snapshot about to be published describes a repository observed some time
/// ago. This closes that gap: if `HEAD`, the index, the working tree, or the
/// rules moved while the build ran, the snapshot describes something that is no
/// longer true.
///
/// Comparing observations rather than plans is deliberate and is stronger, not
/// weaker. A plan is a pure function of the observation and the digest, so two
/// equal observations under an equal digest cannot produce different plans —
/// while two different observations can easily produce the same plan, and that
/// is exactly the case a plan comparison would wave through.
fn confirm_unchanged(
    context: &BuildContext,
    before: Option<&RepoState>,
    digest: [u8; 32],
    cancel: &CancelToken,
) -> Result<()> {
    let now = observe(context, cancel)?;
    if now.as_ref() != before {
        return Err(Error::RepositoryChanged);
    }
    // The observation matching does not settle the rules: an edit to a rule
    // file that was already dirty changes its contents without changing the
    // delta entry that reports it.
    if discovery_digest(context.repo()?.as_ref(), &context.walk, now.as_ref()) != digest {
        return Err(Error::RepositoryChanged);
    }
    Ok(())
}

fn check_cancelled(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

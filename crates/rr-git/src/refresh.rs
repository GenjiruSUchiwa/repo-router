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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rr_core::cancel::CancelToken;
use rr_core::index::{ContentRepresentation, Snapshot};
use rr_core::lang::Lang;
use rr_core::path::RelPath;
use rr_core::refresh::{RefreshMode, RefreshPlan, RefreshReport, ReportedMode};
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
    match prepare(root, threads, mode, cancel)? {
        Refresh::UpToDate { report, .. } => Ok(report),
        Refresh::Prepared(mut prepared) => {
            prepared.publish()?;
            Ok(prepared.finish())
        }
    }
}

/// What a refresh decided, and the means to act on it.
///
/// Split from [`refresh`] so a caller can derive files from the same snapshot
/// while the publication guard is still held. Issue #11's text artifacts have
/// to be written under the guard that published the snapshot they describe;
/// two locks would let a second run publish between them.
#[derive(Debug)]
pub enum Refresh {
    /// Nothing to build. Nothing was locked, created, or written.
    ///
    /// The published snapshot comes back with the report because artifacts
    /// derived from it may still need work even when it does not.
    UpToDate {
        report: RefreshReport,
        snapshot: Arc<Snapshot>,
        /// The working tree the snapshot describes, which is where derived
        /// files belong. Not the directory the caller supplied: that one may be
        /// any subdirectory of the repository, and artifact paths are relative
        /// to the work root alone.
        work_root: PathBuf,
    },
    /// Verified and locked. The snapshot is built but not yet published.
    Prepared(Box<PreparedRefresh>),
}

/// The publication guard, held over the snapshot already on disk.
///
/// For a caller that found the snapshot current but something derived from it
/// out of date, and so needs to write without rebuilding.
#[derive(Debug)]
#[must_use = "the guard is released as soon as this value is dropped"]
pub struct HeldSnapshot {
    _guard: RepositoryWriteGuard,
    work_root: PathBuf,
    snapshot: Arc<Snapshot>,
}

impl HeldSnapshot {
    /// The snapshot that was published when the guard was taken.
    #[must_use]
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The working tree it describes.
    #[must_use]
    pub fn work_root(&self) -> &Path {
        &self.work_root
    }
}

/// Takes the publication guard and reads whatever snapshot is published.
///
/// Re-reads under the lock rather than trusting an earlier load, because the
/// point of taking the guard is that the earlier reading may be stale.
///
/// # Errors
/// Returns [`Error::PublicationLocked`] when another process is publishing, and
/// I/O failures otherwise. A repository with no usable snapshot is `Ok(None)`.
pub fn hold(root: &Path, threads: usize) -> Result<Option<HeldSnapshot>> {
    let context = BuildContext::open(root, threads)?;
    let guard = RepositoryWriteGuard::acquire(&context.work_root)?;
    let store = SnapshotStore::new(&context.work_root);
    let published = Published::load(&store)?;
    Ok(published.snapshot().map(|snapshot| HeldSnapshot {
        snapshot: Arc::clone(snapshot),
        work_root: context.work_root,
        _guard: guard,
    }))
}

/// A built, verified snapshot holding the publication guard.
///
/// The guard is released when this value is dropped, so anything that must
/// happen under it happens before then.
#[derive(Debug)]
#[must_use = "a prepared refresh holds the publication guard and publishes nothing on its own"]
pub struct PreparedRefresh {
    _guard: RepositoryWriteGuard,
    work_root: PathBuf,
    store: SnapshotStore,
    snapshot: Snapshot,
    envelope: Vec<u8>,
    report: RefreshReport,
    started: Instant,
}

impl PreparedRefresh {
    /// The snapshot this run will publish.
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The working tree the snapshot describes.
    ///
    /// Where derived files belong. Not the guard's own path, which names the
    /// lock file inside `.rr/local`.
    #[must_use]
    pub fn work_root(&self) -> &Path {
        &self.work_root
    }

    /// Writes the snapshot, keeping the guard for whatever follows.
    ///
    /// # Errors
    /// Returns I/O failures from the atomic replacement.
    pub fn publish(&mut self) -> Result<()> {
        self.report.snapshot_updated = self.store.publish(&self.envelope)?;
        Ok(())
    }

    /// Releases the guard and returns the report.
    #[must_use]
    pub fn finish(mut self) -> RefreshReport {
        self.report.elapsed_ms = elapsed_ms(self.started);
        self.report
    }
}

/// Builds and verifies a snapshot without publishing it.
///
/// # Errors
/// The same failures as [`refresh`].
pub fn prepare(
    root: &Path,
    threads: usize,
    mode: RefreshMode,
    cancel: &CancelToken,
) -> Result<Refresh> {
    let started = Instant::now();
    let mut report = RefreshReport::default();

    let context = BuildContext::open(root, threads)?;
    let store = SnapshotStore::new(&context.work_root);

    let repo = context.repo()?;

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

    if let Some(snapshot) = published.snapshot() {
        let head_settled =
            snapshot.meta.repo_head_oid == observed.as_ref().and_then(|state| state.head.commit());
        if is_no_op(&store, &planned.plan, head_settled)? {
            record_plan(&mut report, &planned.plan);
            report.elapsed_ms = elapsed_ms(started);
            return Ok(Refresh::UpToDate {
                report,
                snapshot: Arc::clone(snapshot),
                work_root: context.work_root.clone(),
            });
        }
    }

    let guard = RepositoryWriteGuard::acquire(&context.work_root)?;

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

    report.content_reads += planned.content_reads;
    record_plan(&mut report, &plan);

    if let Some(published_snapshot) = published.snapshot() {
        if plan.mode() == RefreshMode::Incremental && plan.is_empty_delta() {
            let mut next = (**published_snapshot).clone();
            let owned: Vec<RelPath> = next
                .files
                .iter()
                .filter_map(|file| next.file_path(file))
                .filter_map(|path| RelPath::new(path).ok())
                .collect();
            let indexed: BTreeSet<&RelPath> = owned.iter().collect();
            next.meta = context.meta_for(&indexed, &[], observed.as_ref())?;
            let envelope = store.encode(&next)?;

            confirm_unchanged(&context, observed.as_ref(), digest, cancel)?;
            check_cancelled(cancel)?;

            return Ok(Refresh::Prepared(Box::new(PreparedRefresh {
                _guard: guard,
                work_root: context.work_root,
                store,
                snapshot: next,
                envelope,
                report,
                started,
            })));
        }
    }

    let built = build(&context, &plan, &published, cancel)?;
    report.reparsed = built.stats().parses;
    report.cached = built.stats().cache_hits;
    report.cache_corrupt = built.stats().cache_corrupt;
    report.degraded = built.stats().degraded;
    report.tags = built.stats().tags;
    report.tags_recovered = built.stats().tags_recovered;
    report.content_reads += built.stats().clean_blob_reads + built.stats().filtered_raw_reads;

    let read = acquired(&published, &plan, &built);

    let outcome = context.assemble(built, observed.as_ref())?;
    let envelope = store.encode(&outcome.snapshot)?;

    confirm_unchanged(&context, observed.as_ref(), digest, cancel)?;
    confirm_content(repo.as_ref(), &read, &mut report.content_reads)?;
    check_cancelled(cancel)?;

    Ok(Refresh::Prepared(Box::new(PreparedRefresh {
        _guard: guard,
        work_root: context.work_root,
        store,
        snapshot: outcome.snapshot,
        envelope,
        report,
        started,
    })))
}

/// Reports "nothing to do" without locking, building, or writing anything.
///
/// Returns `false` when there is work, so the caller continues. The fast path
/// is deliberately narrow: an incremental plan, an empty delta, a `HEAD` the
/// snapshot already names, and a snapshot whose own bytes are on disk.
///
/// `head_settled` is separate from the delta because an empty delta after a
/// commit means no *file* changed, not that nothing did. A snapshot records the
/// commit it was built against; skipping the write would leave that name
/// pointing at a commit further into the past after every generation, and every
/// later run would re-compare against it. It would also make the published
/// bytes disagree with a full rebuild of the same tree, which is the one thing
/// an incremental refresh must never do.
fn is_no_op(store: &SnapshotStore, plan: &RefreshPlan, head_settled: bool) -> Result<bool> {
    if !head_settled || plan.mode() != RefreshMode::Incremental || !plan.is_empty_delta() {
        return Ok(false);
    }

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

    let retainable = published
        .snapshot()
        .filter(|_| plan.mode() == RefreshMode::Incremental);

    context.run(&files, |worker, source| {

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
///
/// The comparison is against the identity the input actually carries rather
/// than against the mere existence of a retention answer, because retention is
/// allowed to fail on its own terms: a pruned fact cache turns a retained file
/// back into a read, and the input that comes back then names bytes somebody
/// read rather than bytes the previous snapshot vouched for.
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
            retainable.and_then(|snapshot| {
                retained(snapshot, plan, &input.path, input.language, input.generated)
            }) != Some((input.oid, input.representation))
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

        return Ok(());
    };

    for (path, oid, representation) in acquired {
        match repo.probe_content(path)? {

            ContentProbe::CleanGitBlob(current) => {
                if current != *oid || *representation != ContentRepresentation::GitCanonical {
                    return Err(Error::RepositoryChanged);
                }
            }

            ContentProbe::ReadRequired => {
                *reads += 1;
                let content = repo
                    .acquire_content(path, ContentProbe::ReadRequired)?
                    .ok_or(Error::RepositoryChanged)?;
                if content.oid != *oid || content.representation != *representation {
                    return Err(Error::RepositoryChanged);
                }
            }
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

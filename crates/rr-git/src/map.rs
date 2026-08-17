//! Building a snapshot of every discovered file.
//!
//! A full build is the degenerate case of a refresh: the plan retains nothing,
//! so every discovered path is processed. Keeping it expressed that way — same
//! pipeline, same assembly, same metadata — is what makes "an incremental
//! refresh must produce the bytes a full build would" a property that can hold
//! rather than a coincidence that has to be maintained.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rr_core::cancel::CancelToken;
use rr_core::index::{
    BuildReport, BuildStats, FileInput, SnapshotBuilder, SnapshotMeta, WorkerStats,
};
use rr_core::parser::Registry;
use rr_core::path::RelPath;
use rr_core::walk::{collected_lang, discover, SourceFile, WalkCfg};
use rr_core::FactCache;

use crate::pipeline::Worker;
use crate::repo::{ChangeKind, RepoState};
use crate::rules::discovery_digest;
use crate::{Error, GitRepo, Result};

/// Builds and returns a deterministic full repository snapshot report.
///
/// # Errors
/// Returns acquisition, cache, extraction, discovery, or index-build failures.
pub fn build_map(root: &Path, threads: usize) -> Result<BuildReport> {
    let context = BuildContext::open(root, threads)?;
    let files = discover(&context.work_root, &context.walk)?;
    let cache = FactCache::open(&context.work_root)?;
    let observed = match context.repo()? {
        Some(repo) => Some(repo.observe_state(&CancelToken::new())?),
        None => None,
    };

    let inputs = context.run(&files, |worker, source| worker.process(source, &cache))?;
    context.assemble(inputs, observed.as_ref())
}

/// How a build divides the paths Git called dirty.
///
/// Three answers, and each is needed by a different part of the next run's
/// planning, which is why they are separated here rather than left as one list
/// the planner re-derives. A path is either something this build indexed from a
/// dirty working tree, something it saw and declined, or something that was not
/// there at all — and only the first two are facts about discovery.
struct DirtyPaths {
    /// Dirty paths this build produced a record for.
    indexed: Vec<RelPath>,
    /// Dirty paths that were on disk and got no record.
    skipped: Vec<RelPath>,
}

/// Sorts an observation's dirty paths by what the build made of each one.
///
/// A path is counted as present unless the observation itself says otherwise.
/// Deletions are absent by definition, and a rename source is absent for the
/// same reason under a different name — treating either as "seen and declined"
/// would let a later run skip a path that had simply not existed yet.
fn dirty_paths(state: &RepoState, indexed: &BTreeSet<&RelPath>) -> DirtyPaths {
    let mut split = DirtyPaths {
        indexed: Vec::new(),
        skipped: Vec::new(),
    };
    for change in &state.changes {
        if indexed.contains(&change.path) {
            split.indexed.push(change.path.clone());
        } else if change.kind != ChangeKind::Deleted {
            split.skipped.push(change.path.clone());
        }
        if let Some(source) = &change.source {
            split.indexed.push(source.clone());
        }
    }
    split
}

/// Whether this path is a plain file right now.
///
/// Guards the one property of a skipped path that can change with no rule
/// changing at all. Discovery declines a path either because a rule excluded it
/// — which a later run notices through the digest — or because what is there is
/// not a file, which nothing announces. A symlink replaced by a real source
/// file is a path discovery would start collecting, so evidence that it was
/// declined must not outlive the symlink it was about.
///
/// Links are not followed, for the same reason the walk does not follow them.
pub(crate) fn is_regular_file(root: &Path, path: &RelPath) -> bool {
    std::fs::symlink_metadata(root.join(path.as_str())).is_ok_and(|meta| meta.is_file())
}

/// The settled answers every build needs before it can process anything: where
/// the work root is, what discovery was asked for, and how many threads may run.
pub struct BuildContext {
    /// The directory paths are relative to — the working tree when there is
    /// one, and the supplied directory when there is not.
    pub work_root: PathBuf,
    /// The discovery configuration this build is running under.
    pub walk: WalkCfg,
    /// Whether the root turned out not to be a Git repository at all.
    pub no_git: bool,
    threads: usize,
}

/// Inputs and the work they cost, which the assembly step needs together.
pub struct BuiltInputs {
    inputs: Vec<FileInput>,
    stats: WorkerStats,
    /// Paths discovery offered that were gone by the time they were read.
    ///
    /// Normally empty. It is kept because "no record" has two causes that look
    /// identical afterwards — discovery declined the path, or the path stopped
    /// existing mid-build — and only the first is evidence about what discovery
    /// collects.
    vanished: Vec<RelPath>,
}

impl BuiltInputs {
    /// The per-file work this build performed.
    #[must_use]
    pub const fn stats(&self) -> &WorkerStats {
        &self.stats
    }

    /// The inputs, for a caller that has to answer for each of them.
    pub fn inputs(&self) -> impl Iterator<Item = &FileInput> {
        self.inputs.iter()
    }

    /// How many files the build produced inputs for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether the build produced no inputs at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

impl BuildContext {
    /// Resolves the root and the discovery configuration one build will use.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the root cannot be canonicalized and
    /// [`Error::Content`] when `threads` is zero.
    pub fn open(root: &Path, threads: usize) -> Result<Self> {
        if threads == 0 {
            return Err(Error::Content(
                "thread count must be greater than zero".into(),
            ));
        }
        let supplied_root = root.canonicalize().map_err(Error::Io)?;
        let repo = GitRepo::discover(&supplied_root)?;
        let (work_root, no_git) = match repo.as_ref() {
            Some(repo) => (repo.workdir().to_path_buf(), false),
            None => (supplied_root, true),
        };

        Ok(Self {
            work_root,
            walk: WalkCfg {
                languages: Some(Registry::indexable()),
                threads: Some(threads),
                ..WalkCfg::default()
            },
            no_git,
            threads,
        })
    }

    /// Opens the repository this build is running against, if there is one.
    ///
    /// Handles are opened per use rather than shared: a `GitRepo` carries an
    /// object-store cache whose value comes from being local to one worker, and
    /// the callers here each ask a single question and drop it.
    ///
    /// # Errors
    /// Returns [`Error::Discover`] when the repository structure is unreadable.
    pub fn repo(&self) -> Result<Option<GitRepo>> {
        GitRepo::discover(&self.work_root)
    }

    /// Runs `work` over every file across the thread pool.
    ///
    /// The caller supplies the per-file decision — process from scratch, or
    /// retain what the previous snapshot already established — and everything
    /// around it stays identical, which is the point.
    ///
    /// # Errors
    /// Returns the first per-file failure, or a thread-pool construction failure.
    pub fn run<F>(&self, files: &[SourceFile], work: F) -> Result<BuiltInputs>
    where
        F: Fn(&mut Worker, &SourceFile) -> Result<(Option<FileInput>, WorkerStats)> + Sync,
    {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.threads)
            .build()
            .map_err(|error| Error::Content(format!("parser pool: {error}")))?;

        let results = pool.install(|| {
            files
                .par_iter()
                .map_init(
                    || Worker::new(&self.work_root),
                    |worker, source| work(worker, source),
                )
                .collect::<Vec<_>>()
        });

        let mut inputs = Vec::with_capacity(results.len());
        let mut stats = WorkerStats::default();
        let mut vanished = Vec::new();
        for (source, result) in files.iter().zip(results) {
            let (input, file_stats) = result?;
            stats.add_assign(file_stats);
            match input {
                Some(input) => inputs.push(input),
                None => vanished.push(source.path.clone()),
            }
        }
        Ok(BuiltInputs {
            inputs,
            stats,
            vanished,
        })
    }

    /// Turns inputs into the finished snapshot and its report.
    ///
    /// The observation is taken whole rather than pre-reduced to a path list.
    /// Two things are derived from it — which paths were dirty and which rule
    /// files the build ran under — and a later run compares both against a
    /// fresh observation, so they have to come from the same one.
    ///
    /// # Errors
    /// Returns index-build failures and `HEAD` lookup failures.
    pub fn assemble(
        &self,
        built: BuiltInputs,
        observed: Option<&RepoState>,
    ) -> Result<BuildReport> {
        let indexed: BTreeSet<&RelPath> = built.inputs.iter().map(|input| &input.path).collect();
        let meta = self.meta_for(&indexed, &built.vanished, observed)?;

        let (snapshot, counts) = SnapshotBuilder::new(meta).build(built.inputs)?;
        let worker = built.stats;
        let stats = BuildStats {
            files: u32::try_from(snapshot.files.len())
                .map_err(|_| Error::Content("file count exceeds u32".into()))?,
            symbols: counts.symbols,
            references: counts.references,
            unresolved_refs: counts.unresolved_refs,
            ambiguous_refs: counts.ambiguous_refs,
            imports: counts.imports,
            unresolved_imports: counts.unresolved_imports,
            ambiguous_imports: counts.ambiguous_imports,
            clean_probes: worker.clean_probes,
            clean_blob_reads: worker.clean_blob_reads,
            filtered_raw_reads: worker.filtered_raw_reads,
            parses: worker.parses,
            complete: worker.complete,
            recovered: worker.recovered,
            tags: worker.tags,
            tags_recovered: worker.tags_recovered,
            degraded: worker.degraded,
            cache_hits: worker.cache_hits,
            cache_misses: worker.cache_misses,
            cache_corrupt: worker.cache_corrupt,
            cache_write_failures: worker.cache_write_failures,
            reparsed: worker.parses,
            content_reads: worker.clean_blob_reads + worker.filtered_raw_reads,
        };
        Ok(BuildReport { snapshot, stats })
    }

    /// Computes the metadata one snapshot would carry, given what was indexed.
    ///
    /// Shared so a republish that never ran a build can still name the commit
    /// it is now true against, using the same split a build would.
    ///
    /// # Errors
    /// Returns `HEAD` lookup failures.
    pub fn meta_for(
        &self,
        indexed: &BTreeSet<&RelPath>,
        vanished: &[RelPath],
        observed: Option<&RepoState>,
    ) -> Result<SnapshotMeta> {
        let repo = self.repo()?;
        let head = repo.as_ref().map(GitRepo::head_oid).transpose()?.flatten();

        let split = observed.map(|state| dirty_paths(state, indexed));
        let (dirty, skipped) = split.map_or_else(Default::default, |split| {
            let skipped = split
                .skipped
                .into_iter()
                .filter(|path| collected_lang(path.as_str(), &self.walk).is_some())
                .filter(|path| !vanished.contains(path))
                .filter(|path| is_regular_file(&self.work_root, path))
                .collect();
            (split.indexed, skipped)
        });

        Ok(SnapshotMeta::new(
            head,
            self.no_git,
            discovery_digest(repo.as_ref(), &self.walk, observed),
        )
        .with_dirty_paths(dirty)
        .with_skipped_paths(skipped))
    }
}

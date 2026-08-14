use std::path::Path;

use rayon::prelude::*;
use rr_core::cache::CacheOutcome;
use rr_core::facts::{Facts, ParseStatus};
use rr_core::index::{
    BuildReport, BuildStats, ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta,
    WorkerStats,
};
use rr_core::lang::Lang;
use rr_core::parser::RustExtractor;
use rr_core::walk::{discover, SourceFile, WalkCfg};
use rr_core::FactCache;

use crate::content::{acquire_non_git, ContentProbe};
use crate::{Error, GitRepo, Result};

/// Builds and returns a deterministic full repository snapshot report.
///
/// # Errors
/// Returns acquisition, cache, extraction, discovery, or index-build failures.
pub fn build_map(root: &Path, threads: usize) -> Result<BuildReport> {
    if threads == 0 {
        return Err(Error::Content(
            "thread count must be greater than zero".into(),
        ));
    }
    let supplied_root = root.canonicalize().map_err(Error::Io)?;
    let main_repo = GitRepo::discover(&supplied_root)?;
    let (work_root, no_git) = match main_repo.as_ref() {
        Some(repo) => (repo.workdir().to_path_buf(), false),
        None => (supplied_root, true),
    };
    let files = discover(
        &work_root,
        &WalkCfg {
            languages: Some(vec![Lang::Rust]),
            threads: Some(threads),
            ..WalkCfg::default()
        },
    )?;
    let cache = FactCache::open(&work_root)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| Error::Content(format!("parser pool: {error}")))?;
    let results = pool.install(|| {
        files
            .par_iter()
            .map_init(
                || new_worker(&work_root),
                |state, source| process_file(state, source, &work_root, &cache),
            )
            .collect::<Vec<_>>()
    });

    let mut inputs = Vec::with_capacity(results.len());
    let mut worker_stats = WorkerStats::default();
    for result in results {
        let (input, stats) = result?;
        worker_stats.add_assign(stats);
        if let Some(input) = input {
            inputs.push(input);
        }
    }

    let head = main_repo
        .as_ref()
        .map(GitRepo::head_oid)
        .transpose()?
        .flatten();
    let meta = SnapshotMeta::new(head, no_git);
    let (snapshot, counts) = SnapshotBuilder::new(meta).build(inputs)?;
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
        clean_probes: worker_stats.clean_probes,
        clean_blob_reads: worker_stats.clean_blob_reads,
        filtered_raw_reads: worker_stats.filtered_raw_reads,
        parses: worker_stats.parses,
        complete: worker_stats.complete,
        recovered: worker_stats.recovered,
        degraded: worker_stats.degraded,
        cache_hits: worker_stats.cache_hits,
        cache_misses: worker_stats.cache_misses,
        cache_corrupt: worker_stats.cache_corrupt,
        cache_write_failures: worker_stats.cache_write_failures,
        reparsed: worker_stats.parses,
        content_reads: worker_stats.clean_blob_reads + worker_stats.filtered_raw_reads,
    };
    Ok(BuildReport { snapshot, stats })
}

struct WorkerState {
    extractor: Option<RustExtractor>,
    extractor_error: Option<String>,
    repo: Option<GitRepo>,
    repo_error: Option<String>,
}

fn new_worker(root: &Path) -> WorkerState {
    let (repo, repo_error) = match GitRepo::discover(root) {
        Ok(repo) => (repo, None),
        Err(error) => (None, Some(error.to_string())),
    };
    match RustExtractor::new() {
        Ok(extractor) => WorkerState {
            extractor: Some(extractor),
            extractor_error: None,
            repo,
            repo_error,
        },
        Err(error) => WorkerState {
            extractor: None,
            extractor_error: Some(error.to_string()),
            repo,
            repo_error,
        },
    }
}

#[allow(
    clippy::similar_names,
    clippy::single_match,
    clippy::match_single_binding,
    clippy::single_match_else
)]
fn process_file(
    state: &mut WorkerState,
    source: &SourceFile,
    root: &Path,
    cache: &FactCache,
) -> Result<(Option<FileInput>, WorkerStats)> {
    if let Some(error) = &state.repo_error {
        return Err(Error::Content(error.clone()));
    }
    let mut stats = WorkerStats::default();
    let acquired = match state.repo.as_ref() {
        Some(repo) => {
            let probe = repo.probe_content(&source.path)?;
            match probe {
                ContentProbe::CleanGitBlob(oid) => {
                    stats.clean_probes += 1;
                    match cached_facts(cache, &rr_core::CacheKey::new(oid, Lang::Rust))? {
                        CacheOutcome::Hit(facts) => {
                            stats.cache_hits += 1;
                            let input = cached_input(source, oid, facts);
                            record_status(&mut stats, input.parse_status);
                            return Ok((Some(input), stats));
                        }
                        CacheOutcome::Miss => stats.cache_misses += 1,
                        CacheOutcome::Corrupt => stats.cache_corrupt += 1,
                    }
                    let Some(content) = repo.acquire_content(&source.path, probe)? else {
                        return Ok((None, stats));
                    };
                    if content.oid != oid {
                        return Err(Error::Content(format!(
                            "clean object identity mismatch for {}",
                            source.path
                        )));
                    }
                    stats.clean_blob_reads += 1;
                    content
                }
                ContentProbe::ReadRequired => {
                    let Some(content) = repo.acquire_content(&source.path, probe)? else {
                        return Ok((None, stats));
                    };
                    stats.filtered_raw_reads += 1;
                    if let Some(input) =
                        cached_after_acquisition(source, &content, cache, &mut stats)?
                    {
                        return Ok((Some(input), stats));
                    }
                    content
                }
            }
        }
        None => {
            let Some(content) = acquire_non_git(root, &source.path)? else {
                return Ok((None, stats));
            };
            stats.filtered_raw_reads += 1;
            if let Some(input) = cached_after_acquisition(source, &content, cache, &mut stats)? {
                return Ok((Some(input), stats));
            }
            content
        }
    };

    let extractor = state.extractor.as_mut().ok_or_else(|| {
        Error::Content(
            state
                .extractor_error
                .clone()
                .unwrap_or_else(|| "Rust extractor construction failed".into()),
        )
    })?;
    let facts = extractor.extract(&acquired.bytes).map_err(Error::Core)?;
    stats.parses += 1;
    record_status(&mut stats, facts.status());
    let key = rr_core::CacheKey::new(acquired.oid, Lang::Rust);
    if cache.put(&key, &facts).is_err() {
        stats.cache_write_failures += 1;
    }
    Ok((
        Some(FileInput {
            path: source.path.clone(),
            oid: acquired.oid,
            representation: acquired.representation,
            generated: source.generated,
            language: source.lang,
            parse_status: facts.status(),
            facts,
        }),
        stats,
    ))
}

fn cached_input(source: &SourceFile, oid: rr_core::Oid, facts: Facts) -> FileInput {
    let status = facts.status();
    FileInput {
        path: source.path.clone(),
        oid,
        representation: ContentRepresentation::GitCanonical,
        generated: source.generated,
        language: source.lang,
        parse_status: status,
        facts,
    }
}

#[allow(clippy::similar_names)]
fn cached_after_acquisition(
    source: &SourceFile,
    content: &crate::AcquiredContent,
    cache: &FactCache,
    stats: &mut WorkerStats,
) -> Result<Option<FileInput>> {
    let key = rr_core::CacheKey::new(content.oid, Lang::Rust);
    match cached_facts(cache, &key)? {
        CacheOutcome::Hit(facts) => {
            stats.cache_hits += 1;
            let status = facts.status();
            record_status(stats, status);
            Ok(Some(FileInput {
                path: source.path.clone(),
                oid: content.oid,
                representation: content.representation,
                generated: source.generated,
                language: source.lang,
                parse_status: status,
                facts,
            }))
        }
        CacheOutcome::Miss => {
            stats.cache_misses += 1;
            Ok(None)
        }
        CacheOutcome::Corrupt => {
            stats.cache_corrupt += 1;
            Ok(None)
        }
    }
}

#[allow(clippy::similar_names)]
fn record_status(stats: &mut WorkerStats, status: ParseStatus) {
    match status {
        ParseStatus::Complete => stats.complete += 1,
        ParseStatus::Recovered { .. } => stats.recovered += 1,
        ParseStatus::Degraded { .. } => stats.degraded += 1,
    }
}
fn cached_facts(cache: &FactCache, key: &rr_core::CacheKey) -> Result<CacheOutcome<Facts>> {
    match cache.get::<Facts>(key) {
        Ok(outcome) => Ok(outcome),
        Err(rr_core::Error::CacheIo { .. }) => Ok(CacheOutcome::Miss),
        Err(error) => Err(Error::Core(error)),
    }
}

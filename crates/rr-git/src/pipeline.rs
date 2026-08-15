//! The one path from a discovered file to an index input.
//!
//! Full and incremental builds differ in *which* files they process, never in
//! how. Keeping that promise in one place is what makes the differential oracle
//! — incremental and full producing byte-identical snapshots — testable at all:
//! two implementations that agreed today would drift apart on the first fix
//! applied to only one of them.

use std::path::{Path, PathBuf};

use rr_core::cache::CacheOutcome;
use rr_core::facts::{DegradedReason, Facts, ParseStatus};
use rr_core::index::{ContentRepresentation, FileInput, WorkerStats};
use rr_core::lang::Lang;
use rr_core::parser::{degraded_facts, Registry};
use rr_core::path::RelPath;
use rr_core::walk::SourceFile;
use rr_core::{CacheKey, FactCache, Oid};

use crate::content::{acquire_non_git, ContentProbe};
use crate::{AcquiredContent, Error, GitRepo, Result};

/// What one acquisition attempt settled.
enum Acquired {
    /// The facts were already known and nothing needs parsing.
    Known(FileInput),
    /// The content is in hand and must be parsed.
    Pending(AcquiredContent),
    /// The file is gone. A path that disappears between discovery and
    /// acquisition is a repository that moved on, not a failure: the next
    /// refresh will observe its absence through Git like any other deletion.
    Vanished,
}

/// Whether facts are worth keeping for a later run.
///
/// Facts produced without an extractor describe rr's language support at the
/// moment they were made, not the file. Storing them would keep serving a
/// lexical-only entry after that language gains an extractor, and nothing short
/// of editing the file or wiping the cache would dislodge it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reusable {
    Yes,
    No,
}

/// One worker's share of the per-file work.
///
/// The repository handle is built once per worker rather than once per file.
/// The extractor registry is built lazily per language, so a construction
/// failure surfaces on first use of that language, with its original message.
pub struct Worker {
    registry: Registry,
    repo: std::result::Result<Option<GitRepo>, String>,
    root: PathBuf,
}

impl Worker {
    /// Builds a worker for the repository rooted at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            registry: Registry::new(),
            repo: GitRepo::discover(root).map_err(|error| error.to_string()),
            root: root.to_path_buf(),
        }
    }

    /// The repository this worker sees, or the error that hid it.
    ///
    /// # Errors
    /// Returns [`Error::Content`] carrying the original discovery failure.
    pub fn repo(&self) -> Result<Option<&GitRepo>> {
        match &self.repo {
            Ok(repo) => Ok(repo.as_ref()),
            Err(message) => Err(Error::Content(message.clone())),
        }
    }

    /// Produces the index input for one discovered file.
    ///
    /// Returns `Ok((None, stats))` when the file no longer exists.
    ///
    /// # Errors
    /// Returns acquisition, extraction, or cache failures.
    pub fn process(
        &mut self,
        source: &SourceFile,
        cache: &FactCache,
    ) -> Result<(Option<FileInput>, WorkerStats)> {
        let mut stats = WorkerStats::default();
        let content = match self.acquire(source, cache, &mut stats)? {
            Acquired::Known(input) => {
                record_status(&mut stats, input.parse_status);
                return Ok((Some(input), stats));
            }
            Acquired::Vanished => return Ok((None, stats)),
            Acquired::Pending(content) => content,
        };

        let (facts, reusable) = self.extract(source.lang, &content)?;
        stats.parses += 1;
        record_status(&mut stats, facts.status());

        if reusable == Reusable::Yes
            && cache
                .put(&CacheKey::new(content.oid, source.lang), &facts)
                .is_err()
        {
            // A cache that cannot be written still lets this run finish; only
            // the next one pays for it. Failing here would turn a full disk
            // into an unusable tool.
            stats.cache_write_failures += 1;
        }

        Ok((
            Some(input_from(
                source,
                content.oid,
                content.representation,
                facts,
            )),
            stats,
        ))
    }

    /// Rebuilds the input for a file the plan says has not changed.
    ///
    /// Retention is what makes an incremental refresh cheap, and it rests
    /// entirely on Git having certified the path as unchanged. What it still
    /// needs is the file's *facts*, which live in the fact cache — so a pruned
    /// or cold cache turns a retained file back into a read and a parse. That
    /// is correct, merely expensive, and the counters say which one happened.
    ///
    /// # Errors
    /// Returns acquisition or extraction failures from the fallback path.
    pub fn retain(
        &mut self,
        source: &SourceFile,
        oid: Oid,
        representation: ContentRepresentation,
        cache: &FactCache,
    ) -> Result<(Option<FileInput>, WorkerStats)> {
        let mut stats = WorkerStats::default();
        if let Some(facts) = lookup(cache, oid, source.lang, &mut stats)? {
            let input = input_from(source, oid, representation, facts);
            record_status(&mut stats, input.parse_status);
            return Ok((Some(input), stats));
        }

        // The lookup already counted why retention failed, and rebuilding
        // counts its own work from zero. Both halves belong to this path, so
        // the fallback's work is added to what has been counted rather than
        // replacing it — a plain `return self.process(..)` here would silently
        // discard the evidence that a retention was even attempted.
        let (input, rebuilt) = self.process(source, cache)?;
        stats.add_assign(rebuilt);
        Ok((input, stats))
    }

    /// Obtains the file's content, or the facts that make reading it needless.
    fn acquire(
        &self,
        source: &SourceFile,
        cache: &FactCache,
        stats: &mut WorkerStats,
    ) -> Result<Acquired> {
        let Some(repo) = self.repo()? else {
            return self.acquire_without_git(source, cache, stats);
        };

        match repo.probe_content(&source.path)? {
            // Git already knows this file's identity, so its facts can be
            // looked up before a single byte is read. This is the whole reason
            // a clean repository costs nothing.
            ContentProbe::CleanGitBlob(oid) => {
                stats.clean_probes += 1;
                if let Some(facts) = lookup(cache, oid, source.lang, stats)? {
                    return Ok(Acquired::Known(input_from(
                        source,
                        oid,
                        ContentRepresentation::GitCanonical,
                        facts,
                    )));
                }

                let probe = ContentProbe::CleanGitBlob(oid);
                let Some(content) = repo.acquire_content(&source.path, probe)? else {
                    return Ok(Acquired::Vanished);
                };
                // The probe promised these bytes. If the object store hands
                // back different ones, the identity that names them is wrong,
                // and everything keyed on it afterwards would be wrong too.
                if content.oid != oid {
                    return Err(Error::Content(format!(
                        "clean object identity mismatch for {}",
                        source.path
                    )));
                }
                stats.clean_blob_reads += 1;
                Ok(Acquired::Pending(content))
            }
            // The identity is only knowable by reading, so the cache can only
            // be consulted afterwards — and often still hits, because an edit
            // that restores previous content restores its identity too.
            ContentProbe::ReadRequired => {
                let Some(content) =
                    repo.acquire_content(&source.path, ContentProbe::ReadRequired)?
                else {
                    return Ok(Acquired::Vanished);
                };
                stats.filtered_raw_reads += 1;
                Ok(known_or_pending(source, content, cache, stats)?)
            }
        }
    }

    /// The same acquisition where there is no Git to ask.
    fn acquire_without_git(
        &self,
        source: &SourceFile,
        cache: &FactCache,
        stats: &mut WorkerStats,
    ) -> Result<Acquired> {
        let Some(content) = acquire_non_git(&self.root, &source.path)? else {
            return Ok(Acquired::Vanished);
        };
        stats.filtered_raw_reads += 1;
        known_or_pending(source, content, cache, stats)
    }

    fn extract(&mut self, lang: Lang, content: &AcquiredContent) -> Result<(Facts, Reusable)> {
        match self.registry.for_lang(lang) {
            Some(Ok(extractor)) => extractor
                .extract(&content.bytes)
                .map(|facts| (facts, Reusable::Yes))
                .map_err(Error::Core),
            Some(Err(message)) => Err(Error::Content(message)),
            None => Ok((
                degraded_facts(&content.bytes, DegradedReason::ParserReturnedNone),
                Reusable::No,
            )),
        }
    }
}

/// Turns freshly acquired content into either known facts or work to do.
fn known_or_pending(
    source: &SourceFile,
    content: AcquiredContent,
    cache: &FactCache,
    stats: &mut WorkerStats,
) -> Result<Acquired> {
    match lookup(cache, content.oid, source.lang, stats)? {
        Some(facts) => Ok(Acquired::Known(input_from(
            source,
            content.oid,
            content.representation,
            facts,
        ))),
        None => Ok(Acquired::Pending(content)),
    }
}

/// Looks up cached facts, counting the outcome.
///
/// A corrupt entry and a missing one lead to the same work, so they return the
/// same thing; they are counted apart because only one of them means something
/// went wrong.
fn lookup(
    cache: &FactCache,
    oid: Oid,
    lang: Lang,
    stats: &mut WorkerStats,
) -> Result<Option<Facts>> {
    match cached_facts(cache, &CacheKey::new(oid, lang))? {
        CacheOutcome::Hit(facts) => {
            stats.cache_hits += 1;
            Ok(Some(facts))
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

/// Reads cached facts, treating an unreadable cache as an empty one.
///
/// A cache I/O failure is a performance problem, not a correctness one: the
/// facts can always be recomputed from content that is still on disk.
fn cached_facts(cache: &FactCache, key: &CacheKey) -> Result<CacheOutcome<Facts>> {
    match cache.get::<Facts>(key) {
        Ok(outcome) => Ok(outcome),
        Err(rr_core::Error::CacheIo { .. }) => Ok(CacheOutcome::Miss),
        Err(error) => Err(Error::Core(error)),
    }
}

/// Assembles the index input. Every construction goes through here so that a
/// new field cannot be filled differently on the cached and parsed paths.
fn input_from(
    source: &SourceFile,
    oid: Oid,
    representation: ContentRepresentation,
    facts: Facts,
) -> FileInput {
    let parse_status = facts.status();
    FileInput {
        path: source.path.clone(),
        oid,
        representation,
        generated: source.generated,
        language: source.lang,
        parse_status,
        facts,
    }
}

fn record_status(counts: &mut WorkerStats, status: ParseStatus) {
    match status {
        ParseStatus::Complete => counts.complete += 1,
        ParseStatus::Recovered { .. } => counts.recovered += 1,
        ParseStatus::Degraded { .. } => counts.degraded += 1,
    }
}

/// A discovered file reduced to what the pipeline needs, for callers that know
/// the path without having walked to it.
#[must_use]
pub fn source_file(path: RelPath, lang: Lang, generated: bool) -> SourceFile {
    SourceFile {
        path,
        lang,
        generated,
    }
}

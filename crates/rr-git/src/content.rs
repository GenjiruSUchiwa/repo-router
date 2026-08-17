//! Exact content acquisition: one identity-and-bytes contract for the map pipeline.
//!
//! The bytes passed to [`RustExtractor`](rr_core::parser::RustExtractor) and the
//! OID stored in the snapshot MUST name the same buffer. This module is the only
//! place that derives a file's content identity; no caller may supply both an OID
//! and bytes that could disagree.
//!
//! Serving source to a caller reuses those same branches — object database,
//! convert-to-Git, raw — through [`acquire_for_source`], which adds the two
//! guarantees indexing does not need: the entry is reached without following a
//! symlink, and the identity can be re-derived immediately before output.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rr_core::oid::HashAlgo;
use rr_core::oid::Oid;
use rr_core::path::RelPath;
use rr_core::verify::{AcquiredSource, ContentPathState, Revalidation, MAX_VERIFIED_CONTENT_BYTES};

use crate::oid::hash_blob;
use crate::safe_open::{self, OpenOutcome, OpenedFile};
use crate::{Error, GitRepo, Result};

/// How the map pipeline should acquire a file's content identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentProbe {
    /// File is tracked, unconflicted, stat-clean: identity is the index OID and
    /// the canonical blob bytes can be read directly from the object database.
    CleanGitBlob(Oid),
    /// Identity is not known up front: bytes must be read and (in Git) filtered
    /// before the OID can be computed.
    ReadRequired,
}

pub use rr_core::content::AcquiredContent;
pub use rr_core::index::ContentRepresentation;

/// A verification-grade acquisition, or the reason there is nothing to verify.
///
/// A refusal is an expected state of the working tree, not a failure: the path
/// no longer holds a regular file that could be this snapshot's source.
#[derive(Debug)]
pub enum AcquireOutcome {
    /// Canonical content and the single OID that names it.
    Acquired(AcquiredContent),
    /// The path cannot yield source in its current state.
    Refused(ContentPathState),
}

impl AcquireOutcome {
    /// Borrows this outcome as the input
    /// [`verify_source`](rr_core::verify::verify_source) expects.
    ///
    /// The borrow is deliberate: the caller keeps the acquisition alive so the
    /// same bytes it verified are the bytes it later serves.
    #[must_use]
    pub const fn as_source(&self) -> AcquiredSource<'_> {
        match self {
            Self::Acquired(content) => AcquiredSource::Acquired(content),
            Self::Refused(state) => AcquiredSource::Refused(*state),
        }
    }
}

impl GitRepo {
    /// Probes the cheapest safe identity for `path`.
    ///
    /// Returns [`ContentProbe::CleanGitBlob`] only when `path` is a tracked,
    /// unconflicted, stat-clean regular file (symlinks excluded). Otherwise
    /// returns [`ContentProbe::ReadRequired`].
    ///
    /// # Errors
    /// Propagates Git index inspection errors.
    pub fn probe_content(&self, path: &RelPath) -> Result<ContentProbe> {
        if let Some(oid) = self.index_oid(path)? {
            Ok(ContentProbe::CleanGitBlob(oid))
        } else {
            Ok(ContentProbe::ReadRequired)
        }
    }

    /// Acquires the exact bytes named by `probe`, deriving identity in one place.
    ///
    /// For [`ContentProbe::CleanGitBlob`] the bytes are read from the object
    /// database by OID and never re-filtered. For [`ContentProbe::ReadRequired`]
    /// the Git clean-filter pipeline runs once; a filter failure is an error,
    /// never a silent fall back to raw bytes. Returns `Ok(None)` when the file
    /// vanished between discovery and read, so callers can skip it.
    ///
    /// # Errors
    /// Returns [`Error::Content`] on object read or filter-pipeline failure.
    pub fn acquire_content(
        &self,
        path: &RelPath,
        probe: ContentProbe,
    ) -> Result<Option<AcquiredContent>> {
        let full = self.workdir().join(path.as_str());
        match probe {
            ContentProbe::CleanGitBlob(oid) => Ok(Some(self.blob_content(oid)?)),
            ContentProbe::ReadRequired => {
                let Some(file) = open_existing(&full)? else {
                    return Ok(None);
                };
                let bytes = self.convert_to_git(file, path)?;
                Ok(Some(git_canonical(bytes, self.hash_algo())))
            }
        }
    }

    /// Returns the current `HEAD` commit OID, or `None` for an unborn repository.
    ///
    /// # Errors
    /// Propagates OID decoding errors (unexpected for a real commit).
    pub fn head_oid(&self) -> Result<Option<Oid>> {
        let mut head = self
            .gix_repo()
            .head()
            .map_err(|error| Error::Content(format!("HEAD lookup failed: {error}")))?;
        if head.is_unborn() {
            return Ok(None);
        }
        let commit = head
            .peel_to_commit()
            .map_err(|error| Error::Content(format!("HEAD commit lookup failed: {error}")))?;
        Ok(Some(Oid::from_raw(commit.id.as_bytes())?))
    }

    /// Runs the repository's clean-filter pipeline over `source`.
    ///
    /// This is the single convert-to-Git implementation: identity computation,
    /// map-pipeline acquisition, and source acquisition all reach the filter
    /// through here, so none of them can normalize content differently.
    ///
    /// A driver that closes stdin before consuming the blob is survivable:
    /// `gix-filter` treats the resulting `EPIPE` as success when the driver
    /// itself exited 0. That only holds if the write is allowed to fail. The
    /// process-wide SIGPIPE disposition `rr` installs for its own stdout would
    /// otherwise kill the run, so the ignore is held for this call only.
    ///
    /// # Errors
    /// Returns [`Error::Content`] when the pipeline is unavailable or a filter
    /// fails, and [`Error::Io`] when the converted stream cannot be read.
    pub(crate) fn convert_to_git(&self, source: impl Read, rel: &RelPath) -> Result<Vec<u8>> {
        counters::record_conversion();
        let _sigpipe = crate::sigpipe::ignore();
        let (mut pipeline, index) = self
            .gix_repo()
            .filter_pipeline(None)
            .map_err(|error| Error::Content(format!("git filter pipeline unavailable: {error}")))?;
        let mut converted = pipeline
            .convert_to_git(source, Path::new(rel.as_str()), &index)
            .map_err(|error| Error::Content(format!("git clean-filter failed: {error}")))?;
        let mut content = Vec::new();
        converted.read_to_end(&mut content).map_err(Error::Io)?;
        Ok(content)
    }

    /// Reads a blob from the object database as canonical content.
    fn blob_content(&self, oid: Oid) -> Result<AcquiredContent> {
        counters::record_odb_read();
        let object = self
            .gix_repo()
            .find_object(object_id(oid)?)
            .map_err(|e| Error::Content(format!("object lookup failed: {e}")))?;
        Ok(AcquiredContent {
            oid,
            representation: ContentRepresentation::GitCanonical,
            bytes: object.detach().data,
        })
    }

    /// Reads a blob's length from its object header, without its content.
    fn blob_size(&self, oid: Oid) -> Result<u64> {
        let header = self
            .gix_repo()
            .find_header(object_id(oid)?)
            .map_err(|e| Error::Content(format!("object header lookup failed: {e}")))?;
        Ok(header.size())
    }

    /// Acquires canonical content for source verification from an already-opened
    /// handle, preferring the object database when the index proves it safe.
    fn acquire_source(&self, path: &RelPath, opened: &OpenedFile) -> Result<AcquireOutcome> {

        if let Some(oid) = self.index_oid(path)? {
            if self.blob_size(oid)? > MAX_VERIFIED_CONTENT_BYTES {
                return Ok(AcquireOutcome::Refused(ContentPathState::TooLarge));
            }
            return Ok(AcquireOutcome::Acquired(self.blob_content(oid)?));
        }

        let Some(worktree) = read_capped(opened)? else {
            return Ok(AcquireOutcome::Refused(ContentPathState::TooLarge));
        };
        let canonical = self.convert_to_git(worktree.as_slice(), path)?;

        if byte_len(&canonical) > MAX_VERIFIED_CONTENT_BYTES {
            return Ok(AcquireOutcome::Refused(ContentPathState::TooLarge));
        }
        Ok(AcquireOutcome::Acquired(git_canonical(
            canonical,
            self.hash_algo(),
        )))
    }
}

/// Acquires raw content from a non-Git directory.
///
/// Bytes are read once and identified with Git blob framing and SHA-1. The OID
/// is a local content identity only ([`ContentRepresentation::RawNoGit`]).
/// Returns `Ok(None)` when the file vanished between discovery and read.
///
/// # Errors
/// Propagates filesystem read errors.
pub fn acquire_non_git(root: &Path, path: &RelPath) -> Result<Option<AcquiredContent>> {
    let full: PathBuf = root.join(path.as_str());
    let bytes = match std::fs::read(&full) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::Io(err)),
    };
    Ok(Some(raw_no_git(bytes)))
}

/// Acquires the canonical content of `path` for verified source output.
///
/// The entry is opened beneath `root` without following a single symlink, and
/// the handle stays open for the whole acquisition so the bytes cannot come
/// from an entry that replaced it. `root` is the repository worktree whenever
/// `repo` is `Some`, matching how the snapshot's paths were recorded.
///
/// # Errors
/// Returns [`Error::Io`] for permission and other I/O failures, and
/// [`Error::Content`] when Git object or filter access fails. Expected
/// worktree states are [`AcquireOutcome::Refused`], not errors.
pub fn acquire_for_source(
    repo: Option<&GitRepo>,
    root: &Path,
    path: &RelPath,
) -> Result<AcquireOutcome> {
    counters::record_acquisition();
    let root = repo.map_or(root, GitRepo::workdir);
    let opened = match safe_open::open_regular_file(root, path)? {
        OpenOutcome::Opened(opened) => opened,
        OpenOutcome::Refused(state) => return Ok(AcquireOutcome::Refused(state)),
    };
    if let Some(repo) = repo {
        return repo.acquire_source(path, &opened);
    }
    let Some(bytes) = read_capped(&opened)? else {
        return Ok(AcquireOutcome::Refused(ContentPathState::TooLarge));
    };
    Ok(AcquireOutcome::Acquired(raw_no_git(bytes)))
}

/// Re-derives the canonical identity of `path` and compares it with `acquired`.
///
/// This is the last check before content is served. It repeats the whole
/// acquisition rather than comparing recorded metadata, because a same-size,
/// same-timestamp overwrite is exactly the case a metadata comparison misses;
/// [`Revalidation::Fresh`] therefore means the bytes already held still are the
/// content this path yields.
///
/// # Errors
/// Same as [`acquire_for_source`].
pub fn revalidate_source(
    repo: Option<&GitRepo>,
    root: &Path,
    path: &RelPath,
    acquired: &AcquiredContent,
) -> Result<Revalidation> {
    match acquire_for_source(repo, root, path)? {
        AcquireOutcome::Refused(state) => Ok(Revalidation::Refused(state)),
        AcquireOutcome::Acquired(current) => {
            let unchanged =
                current.oid == acquired.oid && current.representation == acquired.representation;
            if unchanged {
                Ok(Revalidation::Fresh)
            } else {
                Ok(Revalidation::Refused(ContentPathState::Raced))
            }
        }
    }
}

/// Reads at most one byte past the verification cap, so growth is detected
/// rather than silently truncated into a prefix that would hash to nothing.
///
/// Returns `Ok(None)` when the entry exceeds the cap.
fn read_capped(opened: &OpenedFile) -> Result<Option<Vec<u8>>> {
    let reported = opened.stat.size();
    if reported > MAX_VERIFIED_CONTENT_BYTES {
        return Ok(None);
    }
    counters::record_worktree_read();
    let mut bytes = Vec::with_capacity(usize::try_from(reported).unwrap_or(0));
    (&opened.file)
        .take(MAX_VERIFIED_CONTENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::Io)?;
    if byte_len(&bytes) > MAX_VERIFIED_CONTENT_BYTES {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Opens a file that is expected to exist, reporting removal as `Ok(None)`.
fn open_existing(full: &Path) -> Result<Option<File>> {
    match File::open(full) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::Io(err)),
    }
}

fn git_canonical(bytes: Vec<u8>, algo: HashAlgo) -> AcquiredContent {
    AcquiredContent {
        oid: hash_blob(&bytes, algo),
        representation: ContentRepresentation::GitCanonical,
        bytes,
    }
}

fn raw_no_git(bytes: Vec<u8>) -> AcquiredContent {
    AcquiredContent {
        oid: hash_blob(&bytes, HashAlgo::Sha1),
        representation: ContentRepresentation::RawNoGit,
        bytes,
    }
}

fn byte_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

pub(crate) fn object_id(oid: Oid) -> Result<gix::ObjectId> {
    gix::ObjectId::try_from(oid.as_bytes())
        .map_err(|e| Error::Content(format!("invalid object id: {e}")))
}

/// Work counters proving which acquisition branch ran.
///
/// The fast-path and no-content guarantees are about work *not* done, which no
/// amount of output inspection can show. Counters are thread-local and only
/// compiled for this crate's own tests; recording is a no-op elsewhere.
pub(crate) mod counters {
    #[cfg(test)]
    use std::cell::Cell;

    macro_rules! work_counter {
        ($cell:ident, $record:ident, $read:ident) => {
            #[cfg(test)]
            thread_local! {
                static $cell: Cell<u32> = const { Cell::new(0) };
            }

            #[cfg(test)]
            pub(crate) fn $record() {
                $cell.with(|count| count.set(count.get().saturating_add(1)));
            }

            #[cfg(not(test))]
            pub(crate) const fn $record() {}

            #[cfg(test)]
            pub(crate) fn $read() -> u32 {
                $cell.with(Cell::get)
            }
        };
    }

    work_counter!(ODB_READS, record_odb_read, odb_reads);
    work_counter!(WORKTREE_READS, record_worktree_read, worktree_reads);
    work_counter!(CONVERSIONS, record_conversion, conversions);
    work_counter!(ACQUISITIONS, record_acquisition, acquisitions);

    /// Zeroes every counter for the current thread.
    #[cfg(test)]
    pub(crate) fn reset() {
        ODB_READS.with(|count| count.set(0));
        WORKTREE_READS.with(|count| count.set(0));
        CONVERSIONS.with(|count| count.set(0));
        ACQUISITIONS.with(|count| count.set(0));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn non_git_acquisition_hashes_the_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("src")).unwrap();
        let bytes = b"fn main() {}\n";
        std::fs::write(directory.path().join("src/lib.rs"), bytes).unwrap();
        let path = RelPath::new("src/lib.rs").unwrap();
        let acquired = acquire_non_git(directory.path(), &path).unwrap().unwrap();
        assert_eq!(acquired.bytes, bytes);
        assert_eq!(acquired.oid, hash_blob(bytes, HashAlgo::Sha1));
        assert_eq!(acquired.representation, ContentRepresentation::RawNoGit);
    }

    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    const SOURCE: &str = "src/token.rs";

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?} failed");
    }

    /// A repository with `SOURCE` committed and clean.
    fn committed_repo() -> (TempDir, GitRepo) {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(temp.path(), &["config", "user.name", "Test"]);
        git(temp.path(), &["config", "user.email", "test@example.com"]);
        write(temp.path(), SOURCE, b"fn parse() {}\n");
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-qm", "seed"]);
        let repo = GitRepo::discover(temp.path()).unwrap().unwrap();
        counters::reset();
        (temp, repo)
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, bytes).unwrap();
        full
    }

    fn source() -> RelPath {
        RelPath::new(SOURCE).unwrap()
    }

    fn acquire(repo: &GitRepo, path: &RelPath) -> AcquireOutcome {
        acquire_for_source(Some(repo), repo.workdir(), path).unwrap()
    }

    #[track_caller]
    fn expect_acquired(outcome: AcquireOutcome) -> AcquiredContent {
        match outcome {
            AcquireOutcome::Acquired(content) => content,
            AcquireOutcome::Refused(state) => panic!("expected content, refused as {state:?}"),
        }
    }

    #[track_caller]
    fn expect_refused(outcome: AcquireOutcome) -> ContentPathState {
        match outcome {
            AcquireOutcome::Refused(state) => state,
            AcquireOutcome::Acquired(content) => {
                panic!("expected a refusal, got {} bytes", content.bytes.len())
            }
        }
    }

    #[test]
    fn clean_tracked_source_comes_from_the_object_database() {
        let (_temp, repo) = committed_repo();

        let content = expect_acquired(acquire(&repo, &source()));

        assert_eq!(content.bytes, b"fn parse() {}\n");
        assert_eq!(content.oid, repo.index_oid(&source()).unwrap().unwrap());
        assert_eq!(content.representation, ContentRepresentation::GitCanonical);
        assert_eq!(counters::odb_reads(), 1);
        assert_eq!(counters::worktree_reads(), 0, "clean file was read anyway");
        assert_eq!(counters::conversions(), 0, "clean file was filtered anyway");
    }

    #[test]
    fn modified_source_is_converted_from_the_opened_handle() {
        let (temp, repo) = committed_repo();
        write(temp.path(), SOURCE, b"fn parse(input: &str) {}\n");

        let content = expect_acquired(acquire(&repo, &source()));

        assert_eq!(content.bytes, b"fn parse(input: &str) {}\n");
        assert_eq!(
            content.oid,
            hash_blob(&content.bytes, repo.hash_algo()),
            "identity must name the bytes that were returned"
        );
        assert_eq!(counters::odb_reads(), 0, "index identity was trusted");
        assert_eq!(counters::worktree_reads(), 1);
        assert_eq!(counters::conversions(), 1);
    }

    #[test]
    fn a_stale_index_timestamp_never_serves_the_recorded_identity() {
        let (temp, repo) = committed_repo();

        let index = std::fs::OpenOptions::new()
            .write(true)
            .open(temp.path().join(".git/index"))
            .unwrap();
        index
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();

        let content = expect_acquired(acquire(&repo, &source()));

        assert_eq!(counters::odb_reads(), 0, "racy entry was trusted");
        assert_eq!(counters::worktree_reads(), 1);
        assert_eq!(content.bytes, b"fn parse() {}\n");
    }

    #[test]
    fn crlf_worktree_bytes_acquire_their_canonical_lf_identity() {
        let (temp, repo) = committed_repo();
        write(temp.path(), ".gitattributes", b"*.rs text eol=crlf\n");
        let untracked = RelPath::new("src/added.rs").unwrap();
        write(temp.path(), "src/added.rs", b"fn a() {}\r\nfn b() {}\r\n");

        let content = expect_acquired(acquire(&repo, &untracked));

        assert_eq!(content.bytes, b"fn a() {}\nfn b() {}\n");
        assert_eq!(content.oid, hash_blob(&content.bytes, repo.hash_algo()));
    }

    #[test]
    fn a_symlink_is_never_source_even_though_git_hashes_its_target_text() {
        let (temp, repo) = committed_repo();
        let link = RelPath::new("src/link.rs").unwrap();
        std::os::unix::fs::symlink("token.rs", temp.path().join("src/link.rs")).unwrap();

        assert_eq!(
            expect_refused(acquire(&repo, &link)),
            ContentPathState::Symlink
        );
        assert_eq!(counters::worktree_reads(), 0);
    }

    #[test]
    fn a_symlinked_parent_cannot_redirect_the_walk() {
        let (temp, repo) = committed_repo();
        std::os::unix::fs::symlink("src", temp.path().join("alias")).unwrap();
        let through_link = RelPath::new("alias/token.rs").unwrap();

        assert_eq!(
            expect_refused(acquire(&repo, &through_link)),
            ContentPathState::Symlink
        );
    }

    #[test]
    fn missing_directories_and_special_files_are_refused_by_kind() {
        let (temp, repo) = committed_repo();
        let gone = RelPath::new("src/gone.rs").unwrap();
        let directory = RelPath::new("src").unwrap();
        let fifo = RelPath::new("src/pipe").unwrap();
        Command::new("mkfifo")
            .arg(temp.path().join("src/pipe"))
            .output()
            .unwrap();

        assert_eq!(
            expect_refused(acquire(&repo, &gone)),
            ContentPathState::Missing
        );
        assert_eq!(
            expect_refused(acquire(&repo, &directory)),
            ContentPathState::NotRegular
        );
        assert_eq!(
            expect_refused(acquire(&repo, &fifo)),
            ContentPathState::NotRegular,
            "opening a reader-less FIFO must not block"
        );
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let (temp, repo) = committed_repo();
        let huge = RelPath::new("src/huge.rs").unwrap();
        let file = std::fs::File::create(temp.path().join("src/huge.rs")).unwrap();
        file.set_len(MAX_VERIFIED_CONTENT_BYTES + 1).unwrap();

        assert_eq!(
            expect_refused(acquire(&repo, &huge)),
            ContentPathState::TooLarge
        );
        assert_eq!(counters::worktree_reads(), 0, "oversized file was read");
    }

    #[test]
    fn revalidation_is_fresh_only_while_the_identity_holds() {
        let (temp, repo) = committed_repo();
        let content = expect_acquired(acquire(&repo, &source()));
        let revalidate = |repo: &GitRepo| {
            revalidate_source(Some(repo), repo.workdir(), &source(), &content).unwrap()
        };

        assert_eq!(revalidate(&repo), Revalidation::Fresh);
        assert_eq!(
            counters::acquisitions(),
            2,
            "the final check reacquires rather than trusting recorded metadata"
        );

        write(temp.path(), "src/copy.rs", &content.bytes);
        std::fs::rename(temp.path().join("src/copy.rs"), temp.path().join(SOURCE)).unwrap();
        assert_eq!(revalidate(&repo), Revalidation::Fresh);

        write(temp.path(), SOURCE, b"fn parse(changed: u8) {}\n");
        assert_eq!(
            revalidate(&repo),
            Revalidation::Refused(ContentPathState::Raced)
        );
    }

    #[test]
    fn revalidation_reports_the_state_that_replaced_the_file() {
        let (temp, repo) = committed_repo();
        let content = expect_acquired(acquire(&repo, &source()));
        std::fs::remove_file(temp.path().join(SOURCE)).unwrap();

        assert_eq!(
            revalidate_source(Some(&repo), repo.workdir(), &source(), &content).unwrap(),
            Revalidation::Refused(ContentPathState::Missing)
        );

        std::os::unix::fs::symlink("elsewhere.rs", temp.path().join(SOURCE)).unwrap();
        assert_eq!(
            revalidate_source(Some(&repo), repo.workdir(), &source(), &content).unwrap(),
            Revalidation::Refused(ContentPathState::Symlink)
        );
    }

    #[test]
    fn source_outside_git_is_raw_and_still_symlink_refusing() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"fn parse() {}\n";
        write(temp.path(), SOURCE, bytes);
        std::os::unix::fs::symlink("token.rs", temp.path().join("src/link.rs")).unwrap();

        let content = expect_acquired(acquire_for_source(None, temp.path(), &source()).unwrap());
        assert_eq!(content.representation, ContentRepresentation::RawNoGit);
        assert_eq!(content.oid, hash_blob(bytes, HashAlgo::Sha1));

        let link = RelPath::new("src/link.rs").unwrap();
        assert_eq!(
            expect_refused(acquire_for_source(None, temp.path(), &link).unwrap()),
            ContentPathState::Symlink
        );
    }

    /// Installs a SIGPIPE disposition and restores the one it replaced on drop.
    ///
    /// The disposition is process-wide, so restoring it from a plain statement
    /// at the end of a test is not enough: a failing assertion or a `Result`
    /// that came back `Err` unwinds past that statement and leaves the whole
    /// test binary terminating on `EPIPE`, which turns one reportable failure
    /// into a killed process with no output.
    #[cfg(unix)]
    struct SigpipeDisposition(libc::sighandler_t);

    #[cfg(unix)]
    impl SigpipeDisposition {
        #[allow(unsafe_code)]
        fn install(handler: libc::sighandler_t) -> Self {

            Self(unsafe { libc::signal(libc::SIGPIPE, handler) })
        }
    }

    #[cfg(unix)]
    impl Drop for SigpipeDisposition {
        #[allow(unsafe_code)]
        fn drop(&mut self) {

            unsafe {
                libc::signal(libc::SIGPIPE, self.0);
            }
        }
    }

    /// A driver that stops reading is how #59 used to kill `rr`. The
    /// disposition here is the one `main` installs; without the ignore around
    /// convert-to-Git this process dies before the assertion.
    #[cfg(unix)]
    #[test]
    fn a_clean_filter_that_closes_stdin_returns_what_the_driver_wrote() {
        let (temp, _repo) = committed_repo();
        git(temp.path(), &["config", "filter.trunc.clean", "head -c 8"]);
        write(temp.path(), ".gitattributes", b"*.rs filter=trunc\n");
        let input = format!(
            "pub fn verify_token() -> bool {{ true }}\n// {}\n",
            "x".repeat(256 * 1024)
        );
        write(temp.path(), SOURCE, input.as_bytes());
        let repo = GitRepo::discover(temp.path()).unwrap().unwrap();

        let terminating = SigpipeDisposition::install(libc::SIG_DFL);
        let bytes = repo.convert_to_git(input.as_bytes(), &source()).unwrap();
        drop(terminating);

        assert_eq!(bytes, b"pub fn v");
    }
}

//! Git repository interaction and index-based OID lookup.

mod state;

use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use rr_core::path::RelPath;

use crate::oid::{hash_blob, HashAlgo, Oid};
use crate::{Error, Result};

pub use state::{ChangeKind, HeadState, RepoState, WorktreeChange};

/// A discovered Git repository wrapper optimized for fast index queries.
pub struct GitRepo {
    repo: gix::Repository,
    algo: HashAlgo,
    workdir: PathBuf,
}

impl GitRepo {
    /// Discovers a Git repository starting at `dir` and searching upwards.
    ///
    /// Returns `Ok(None)` if `dir` is not located within a Git repository.
    ///
    /// # Errors
    /// Returns [`Error::Discover`] if an invalid or unreadable Git repository structure is found.
    pub fn discover(dir: &Path) -> Result<Option<Self>> {
        match gix::discover(dir) {
            Ok(repo) => {
                let algo = match repo.object_hash().len_in_bytes() {
                    32 => HashAlgo::Sha256,
                    _ => HashAlgo::Sha1,
                };
                let workdir = repo
                    .workdir()
                    .map_or_else(|| repo.git_dir().to_path_buf(), Path::to_path_buf);

                Ok(Some(Self {
                    repo,
                    algo,
                    workdir,
                }))
            }
            Err(gix::discover::Error::Discover(
                gix::discover::upwards::Error::NoGitRepository { .. }
                | gix::discover::upwards::Error::NoGitRepositoryWithinCeiling { .. },
            )) => Ok(None),
            Err(err) => Err(Error::from(err)),
        }
    }

    /// Returns the hashing algorithm used by this repository.
    #[must_use]
    pub const fn hash_algo(&self) -> HashAlgo {
        self.algo
    }

    /// Returns the underlying [`gix::Repository`] for content-object access.
    pub(crate) fn gix_repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// Returns the working directory path of this repository.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Re-expresses an absolute path relative to this repository's working directory.
    ///
    /// Returns `None` when the path lies outside the working directory. Falls back to
    /// canonicalized comparison so symlinked path prefixes (e.g. macOS temp dirs) still resolve.
    fn workdir_rel(&self, abs: &Path) -> Option<RelPath> {
        fn rel_str(abs: &Path, base: &Path) -> Option<String> {
            let stripped = abs.strip_prefix(base).ok()?;
            Some(stripped.to_str()?.replace(std::path::MAIN_SEPARATOR, "/"))
        }

        let rel = rel_str(abs, &self.workdir).or_else(|| {
            let abs = abs.canonicalize().ok()?;
            let base = self.workdir.canonicalize().ok()?;
            rel_str(&abs, &base)
        })?;
        RelPath::try_from(rel.as_str()).ok()
    }

    /// Retrieves the object identifier from the Git index iff the file is tracked and unmodified.
    ///
    /// Returns:
    /// - `Ok(Some(oid))` if the file is a tracked, unconflicted regular file that is clean
    ///   (stat matches and not racy).
    /// - `Ok(None)` if the file is untracked, modified, racy, conflicted, a symlink, or
    ///   missing from the index.
    ///
    /// # Errors
    /// Returns [`Error::Index`] if opening or reading the Git index fails with a corruption error.
    /// Returns [`Error::Io`] on filesystem permission errors.
    pub fn index_oid(&self, rel: &RelPath) -> Result<Option<Oid>> {
        use gix::index::entry::Flags;

        let index = self.repo.index_or_empty()?;

        let path_bstr = rel.as_str().as_bytes().as_bstr();
        let Some(entry) =
            index.entry_by_path_and_stage(path_bstr, gix::index::entry::Stage::Unconflicted)
        else {
            return Ok(None);
        };

        // An intent-to-add entry records the *empty blob* as a placeholder: Git
        // has never stored this file's content. Returning that OID would name
        // the wrong bytes, and every such file in the repository would collide
        // on one cache key. A skip-worktree entry's stat is equally meaningless,
        // because Git deliberately stopped tracking what is on disk. Neither is
        // an identity, so both fall through to hashing the real content.
        if entry
            .flags
            .intersects(Flags::INTENT_TO_ADD | Flags::SKIP_WORKTREE)
        {
            return Ok(None);
        }

        if entry.mode != gix::index::entry::Mode::FILE
            && entry.mode != gix::index::entry::Mode::FILE_EXECUTABLE
        {
            return Ok(None);
        }

        let full_path = self.workdir.join(rel.as_str());
        let meta = match gix::index::fs::Metadata::from_path_no_follow(&full_path) {
            Ok(m) => m,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::Io(err)),
        };

        if !meta.is_file() {
            return Ok(None);
        }

        let Ok(fs_stat) = gix::index::entry::Stat::from_fs(&meta) else {
            return Ok(None);
        };

        let mut options = self.repo.stat_options().unwrap_or_default();
        options.use_nsec = entry.stat.mtime.nsecs != 0;

        if !entry.stat.matches(&fs_stat, options) {
            return Ok(None);
        }

        let is_racy = std::fs::metadata(index.path())
            .and_then(|m| m.modified())
            .map_or(true, |index_mtime| {
                entry.stat.is_racy(index_mtime.into(), options)
            });

        if is_racy {
            return Ok(None);
        }

        let oid = Oid::from_raw(entry.id.as_bytes())?;
        Ok(Some(oid))
    }

    /// Hashes the worktree content at `full_path` as a Git blob with the repository's
    /// content filters applied (CRLF normalization, `.gitattributes` filters), matching
    /// what `git hash-object` would produce for `rel`.
    ///
    /// Returns `Ok(None)` when the filter pipeline cannot be constructed or applied,
    /// in which case the caller should hash the raw bytes instead.
    fn filtered_blob_oid(&self, rel: &RelPath, full_path: &Path) -> Result<Option<Oid>> {
        let file = std::fs::File::open(full_path).map_err(Error::Io)?;
        match self.convert_to_git(file, rel) {
            Ok(content) => Ok(Some(hash_blob(&content, self.algo))),
            // An unavailable pipeline or a failing filter is not fatal here:
            // the caller hashes the raw bytes instead. Read failures still are.
            Err(Error::Content(_)) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// Computes the OID of a file's current content.
///
/// Performs zero file content reads when the file is clean and tracked in `repo`.
/// Otherwise, hashes the content as a Git blob, applying the repository's content
/// filters when available. Symlinks hash their literal link target text, as Git does.
///
/// The hashing algorithm is the repository's own when `repo` is `Some`, and
/// [`HashAlgo::Sha1`] otherwise.
///
/// # Errors
/// Returns [`Error::Io`] if reading the file from disk fails.
/// Returns [`Error::Index`] if Git index inspection fails.
pub fn oid_of(repo: Option<&GitRepo>, root: &Path, rel: &RelPath) -> Result<Oid> {
    let target_path = root.join(rel.as_str());
    let algo = repo.map_or(HashAlgo::Sha1, GitRepo::hash_algo);
    let repo_rel = repo.and_then(|r| r.workdir_rel(&target_path));

    if let (Some(repo), Some(repo_rel)) = (repo, repo_rel.as_ref()) {
        if let Some(oid) = repo.index_oid(repo_rel)? {
            return Ok(oid);
        }
    }

    let meta = std::fs::symlink_metadata(&target_path)?;
    if meta.file_type().is_symlink() {
        let link = std::fs::read_link(&target_path)?;
        return Ok(hash_blob(link.as_os_str().as_encoded_bytes(), algo));
    }

    if let (Some(repo), Some(repo_rel)) = (repo, repo_rel.as_ref()) {
        if let Some(oid) = repo.filtered_blob_oid(repo_rel, &target_path)? {
            return Ok(oid);
        }
    }

    let content = std::fs::read(&target_path)?;
    Ok(hash_blob(&content, algo))
}

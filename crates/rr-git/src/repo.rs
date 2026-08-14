//! Git repository interaction and index-based OID lookup.

use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;
use rr_core::path::RelPath;

use crate::oid::{hash_blob, HashAlgo, Oid};
use crate::{Error, Result};

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

                Ok(Some(Self { repo, algo, workdir }))
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

    /// Returns the working directory path of this repository.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Retrieves the object identifier from the Git index iff the file is tracked and unmodified.
    ///
    /// Returns:
    /// - `Ok(Some(oid))` if the file is tracked and clean (stat matches and not racy).
    /// - `Ok(None)` if the file is untracked, modified, racy, or missing from the index.
    ///
    /// # Errors
    /// Returns [`Error::Index`] if opening or reading the Git index fails with a corruption error.
    /// Returns [`Error::Io`] on filesystem permission errors.
    pub fn index_oid(&self, rel: &RelPath) -> Result<Option<Oid>> {
        let index = match self.repo.index_or_empty() {
            Ok(idx) => idx,
            Err(err) => return Err(Error::from(err)),
        };

        let path_bstr = rel.as_str().as_bytes().as_bstr();
        let Some(entry) = index.entry_by_path(path_bstr) else {
            return Ok(None);
        };

        let full_path = self.workdir.join(rel.as_str());
        let meta = match gix::index::fs::Metadata::from_path_no_follow(&full_path) {
            Ok(m) => m,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::Io(err)),
        };

        let Ok(fs_stat) = gix::index::entry::Stat::from_fs(&meta) else {
            return Ok(None);
        };

        let mut options = self.repo.stat_options().unwrap_or_default();
        options.use_nsec = true;

        if !entry.stat.matches(&fs_stat, options) {
            return Ok(None);
        }

        let index_path = index.path();
        let is_racy = if let Ok(index_meta) = std::fs::metadata(index_path) {
            if let Ok(index_mtime) = index_meta.modified() {
                entry.stat.is_racy(index_mtime.into(), options)
            } else {
                false
            }
        } else {
            false
        };

        if is_racy {
            return Ok(None);
        }

        let oid = Oid::from_raw(entry.id.as_bytes())?;
        Ok(Some(oid))
    }
}

/// Computes the OID of a file's current content.
///
/// Performs zero file content reads when the file is clean and tracked in `repo`.
/// Otherwise, reads the file from disk and hashes its content as a Git blob.
///
/// # Errors
/// Returns [`Error::Io`] if reading the file from disk fails.
/// Returns [`Error::Index`] if Git index inspection fails.
pub fn oid_of(
    repo: Option<&GitRepo>,
    root: &Path,
    rel: &RelPath,
    algo: HashAlgo,
) -> Result<Oid> {
    if let Some(repo) = repo {
        if let Some(oid) = repo.index_oid(rel)? {
            return Ok(oid);
        }
    }

    let target_path = root.join(rel.as_str());
    let content = std::fs::read(&target_path)?;
    let hash_algo = repo.map_or(algo, GitRepo::hash_algo);
    Ok(hash_blob(&content, hash_algo))
}

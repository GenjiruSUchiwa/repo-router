//! Exact content acquisition: one identity-and-bytes contract for the map pipeline.
//!
//! The bytes passed to [`RustExtractor`](rr_core::parser::RustExtractor) and the
//! OID stored in the snapshot MUST name the same buffer. This module is the only
//! place that derives a file's content identity; no caller may supply both an OID
//! and bytes that could disagree.

use std::io::Read;
use std::path::{Path, PathBuf};

use rr_core::oid::HashAlgo;
use rr_core::oid::Oid;
use rr_core::path::RelPath;

use crate::oid::hash_blob;
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

pub use rr_core::index::ContentRepresentation;

/// Exact content bytes and the single OID that names them.
#[derive(Debug, Clone)]
pub struct AcquiredContent {
    /// Object identifier of `bytes` (Git blob OID, or local content hash).
    pub oid: Oid,
    /// Whether `oid` is a Git-canonical or raw local identity.
    pub representation: ContentRepresentation,
    /// The exact bytes that should be parsed and cached.
    pub bytes: Vec<u8>,
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
            ContentProbe::CleanGitBlob(oid) => {
                let bytes = self.read_blob(oid)?;
                Ok(Some(AcquiredContent {
                    oid,
                    representation: ContentRepresentation::GitCanonical,
                    bytes,
                }))
            }
            ContentProbe::ReadRequired => {
                let Some(bytes) = self.filtered_bytes(&full, path)? else {
                    return Ok(None);
                };
                let oid = hash_blob(&bytes, self.hash_algo());
                Ok(Some(AcquiredContent {
                    oid,
                    representation: ContentRepresentation::GitCanonical,
                    bytes,
                }))
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

    fn read_blob(&self, oid: Oid) -> Result<Vec<u8>> {
        let gid = gix::ObjectId::try_from(oid.as_bytes())
            .map_err(|e| Error::Content(format!("invalid object id: {e}")))?;
        let object = self
            .gix_repo()
            .find_object(gid)
            .map_err(|e| Error::Content(format!("object lookup failed: {e}")))?;
        Ok(object.detach().data)
    }

    fn filtered_bytes(&self, full: &Path, rel: &RelPath) -> Result<Option<Vec<u8>>> {
        let (mut pipeline, index) = self
            .gix_repo()
            .filter_pipeline(None)
            .map_err(|error| Error::Content(format!("git filter pipeline unavailable: {error}")))?;
        let file = match std::fs::File::open(full) {
            Ok(f) => f,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(Error::Io(err)),
        };
        let mut converted = pipeline
            .convert_to_git(file, Path::new(rel.as_str()), &index)
            .map_err(|error| Error::Content(format!("git clean-filter failed: {error}")))?;
        let mut content = Vec::new();
        converted.read_to_end(&mut content).map_err(Error::Io)?;
        Ok(Some(content))
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
    let oid = hash_blob(&bytes, HashAlgo::Sha1);
    Ok(Some(AcquiredContent {
        oid,
        representation: ContentRepresentation::RawNoGit,
        bytes,
    }))
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
}

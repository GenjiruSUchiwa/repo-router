use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::NamedTempFile;

use crate::index::Snapshot;

/// Bumped whenever the snapshot's own layout changes, so a file written by an
/// older binary is rebuilt rather than misread.
///
/// Version 4 added the discovery digest and the two path lists an incremental
/// refresh plans against. An older file decodes without them and would look
/// merely *empty* of dirty paths, which is a lie a reader has no way to detect —
/// the refresh would then leave stale worktree records in place for ever.
/// Version 5 added the per-symbol display signature and the per-file test
/// classification, both of which sit inside their arenas: an older payload
/// decodes its successor's fields out of alignment, so the envelope has to
/// refuse it before postcard is asked to try.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 5;
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"RRIDX\0\0\0";
const HEADER_LEN: usize = 8 + 4 + 8 + 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildReason {
    BadMagic,
    UnsupportedVersion { found: u32 },
    LengthMismatch,
    ChecksumMismatch,
    InvalidPayload,
    TrailingBytes,
    LexicalMismatch,
    BuildVersionMismatch { found: u32 },
    RankingProfileMismatch { found: u32 },
    InvalidInvariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Ready(Arc<Snapshot>),
    Missing,
    NeedsRebuild(RebuildReason),
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotIoError {
    #[error("snapshot I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot payload length exceeds envelope limits")]
    LengthOverflow,
    #[error("snapshot serialization error: {0}")]
    Serialization(#[from] postcard::Error),
}

#[derive(Debug)]
pub struct SnapshotStore {
    root: PathBuf,
    path: PathBuf,
}

impl SnapshotStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            path: crate::workspace::snapshot_path(root),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads and strictly validates the complete snapshot envelope.
    ///
    /// # Errors
    /// Returns an error for missing-parent permissions and unexpected filesystem I/O.
    pub fn load(&self) -> Result<LoadOutcome, SnapshotIoError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadOutcome::Missing);
            }
            Err(source) => {
                return Err(SnapshotIoError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        Ok(decode(&bytes))
    }

    /// Validates and serializes `snapshot` into its on-disk envelope.
    ///
    /// Publication compares bytes rather than structures, so the bytes have to
    /// exist before the decision to write is made. This is the one place that
    /// produces them.
    ///
    /// # Errors
    /// Returns an error when validation or serialization fails.
    pub fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>, SnapshotIoError> {
        snapshot.validate().map_err(|error| SnapshotIoError::Io {
            path: self.path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })?;
        let payload = postcard::to_allocvec(snapshot)?;
        encode(&payload)
    }

    /// Reads the currently published envelope, if there is one.
    ///
    /// # Errors
    /// Returns an error for filesystem failures other than absence.
    pub fn read_published(&self) -> Result<Option<Vec<u8>>, SnapshotIoError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SnapshotIoError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Publishes `envelope` only if it differs from what is already on disk.
    ///
    /// A rebuild that reaches the same bytes is not news, and rewriting the file
    /// anyway would move its mtime and invalidate every reader's belief that
    /// nothing happened. Returns whether the file was replaced.
    ///
    /// # Errors
    /// Returns an error when reading the current file or replacing it fails.
    pub fn publish(&self, envelope: &[u8]) -> Result<bool, SnapshotIoError> {
        if self.read_published()?.as_deref() == Some(envelope) {
            return Ok(false);
        }
        self.write_envelope(envelope)?;
        Ok(true)
    }

    /// Validates, serializes, flushes, and atomically replaces the snapshot file.
    /// Concurrent readers see a complete old or new file; no directory fsync is issued,
    /// so sudden power loss may discard the latest replacement and require a rebuild.
    ///
    /// # Errors
    /// Returns an error when validation, serialization, temporary-file, or replacement I/O fails.
    pub fn write(&self, snapshot: &Snapshot) -> Result<(), SnapshotIoError> {
        let envelope = self.encode(snapshot)?;
        self.write_envelope(&envelope)
    }

    /// The one atomic replacement: same-directory unique temp, then rename.
    fn write_envelope(&self, envelope: &[u8]) -> Result<(), SnapshotIoError> {
        // The ignore mark goes down before the snapshot does, so a snapshot
        // written without a fact cache alongside it is hidden just the same.
        crate::workspace::ensure_private(&self.root).map_err(|source| SnapshotIoError::Io {
            path: crate::workspace::state_dir(&self.root),
            source,
        })?;

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| SnapshotIoError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let mut temp = NamedTempFile::new_in(parent).map_err(|source| SnapshotIoError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        temp.write_all(envelope)
            .map_err(|source| SnapshotIoError::Io {
                path: temp.path().to_path_buf(),
                source,
            })?;
        temp.flush().map_err(|source| SnapshotIoError::Io {
            path: temp.path().to_path_buf(),
            source,
        })?;
        temp.persist(&self.path)
            .map_err(|error| SnapshotIoError::Io {
                path: self.path.clone(),
                source: error.error,
            })?;
        Ok(())
    }
}

fn encode(payload: &[u8]) -> Result<Vec<u8>, SnapshotIoError> {
    let checksum = blake3::hash(payload);
    let length = u64::try_from(payload.len()).map_err(|_| SnapshotIoError::LengthOverflow)?;
    let capacity = HEADER_LEN
        .checked_add(payload.len())
        .ok_or(SnapshotIoError::LengthOverflow)?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(&SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_SCHEMA_VERSION.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(checksum.as_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> LoadOutcome {
    if bytes.len() < HEADER_LEN {
        return LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch);
    }
    if bytes[..8] != SNAPSHOT_MAGIC {
        return LoadOutcome::NeedsRebuild(RebuildReason::BadMagic);
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4]));
    if version != SNAPSHOT_SCHEMA_VERSION {
        return LoadOutcome::NeedsRebuild(RebuildReason::UnsupportedVersion { found: version });
    }
    let length = match bytes[12..20].try_into() {
        Ok(raw) => u64::from_le_bytes(raw),
        Err(_) => return LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch),
    };
    let Ok(length) = usize::try_from(length) else {
        return LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch);
    };
    let Some(end) = HEADER_LEN.checked_add(length) else {
        return LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch);
    };
    if bytes.len() < end {
        return LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch);
    }
    if bytes.len() > end {
        return LoadOutcome::NeedsRebuild(RebuildReason::TrailingBytes);
    }
    let expected = &bytes[20..52];
    let actual = blake3::hash(&bytes[HEADER_LEN..]);
    if expected != actual.as_bytes() {
        return LoadOutcome::NeedsRebuild(RebuildReason::ChecksumMismatch);
    }
    let parsed = postcard::take_from_bytes::<Snapshot>(&bytes[HEADER_LEN..]);
    let Ok((snapshot, remainder)) = parsed else {
        return LoadOutcome::NeedsRebuild(RebuildReason::InvalidPayload);
    };
    if !remainder.is_empty() {
        return LoadOutcome::NeedsRebuild(RebuildReason::TrailingBytes);
    }
    if snapshot.meta.lexical_profile != crate::lex::lexical_profile() {
        return LoadOutcome::NeedsRebuild(RebuildReason::LexicalMismatch);
    }
    if snapshot.meta.build_version != crate::index::BUILD_VERSION {
        return LoadOutcome::NeedsRebuild(RebuildReason::BuildVersionMismatch {
            found: snapshot.meta.build_version,
        });
    }
    if !snapshot
        .meta
        .ranking
        .accepts(&crate::ranking::DEFAULT_RANKING_PROFILE)
    {
        return LoadOutcome::NeedsRebuild(RebuildReason::RankingProfileMismatch {
            found: snapshot.meta.ranking.profile_version,
        });
    }
    if snapshot.validate().is_err() {
        return LoadOutcome::NeedsRebuild(RebuildReason::InvalidInvariant);
    }
    LoadOutcome::Ready(Arc::new(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_payload() {
        let payload = b"payload";
        let encoded = encode(payload).unwrap();
        assert_eq!(encoded.len(), HEADER_LEN + payload.len());
    }

    #[test]
    fn malformed_headers_need_rebuild() {
        assert!(matches!(
            decode(&[]),
            LoadOutcome::NeedsRebuild(RebuildReason::LengthMismatch)
        ));
        let mut bytes = vec![0; HEADER_LEN];
        bytes[..8].copy_from_slice(b"badmagic");
        assert!(matches!(
            decode(&bytes),
            LoadOutcome::NeedsRebuild(RebuildReason::BadMagic)
        ));
    }
    #[test]
    fn envelope_trailing_bytes_need_rebuild() {
        let mut bytes = encode(b"payload").unwrap();
        bytes.push(1);
        assert!(matches!(
            decode(&bytes),
            LoadOutcome::NeedsRebuild(RebuildReason::TrailingBytes)
        ));
    }
    #[test]
    fn lexical_profile_mismatch_needs_rebuild() {
        let (mut snapshot, _) = crate::index::SnapshotBuilder::new(
            crate::index::SnapshotMeta::new(None, true, [0; 32]),
        )
        .build(Vec::new())
        .unwrap();
        snapshot.meta.lexical_profile.algorithm += 1;
        let payload = postcard::to_allocvec(&snapshot).unwrap();
        let bytes = encode(&payload).unwrap();
        assert!(matches!(
            decode(&bytes),
            LoadOutcome::NeedsRebuild(RebuildReason::LexicalMismatch)
        ));
    }
    #[test]
    fn build_version_mismatch_needs_rebuild() {
        let (mut snapshot, _) = crate::index::SnapshotBuilder::new(
            crate::index::SnapshotMeta::new(None, true, [0; 32]),
        )
        .build(Vec::new())
        .unwrap();
        snapshot.meta.build_version += 1;
        let payload = postcard::to_allocvec(&snapshot).unwrap();
        let bytes = encode(&payload).unwrap();
        assert!(matches!(
            decode(&bytes),
            LoadOutcome::NeedsRebuild(RebuildReason::BuildVersionMismatch { .. })
        ));
    }
    #[test]
    fn ranking_profile_mismatch_needs_rebuild() {
        let (mut snapshot, _) = crate::index::SnapshotBuilder::new(
            crate::index::SnapshotMeta::new(None, true, [0; 32]),
        )
        .build(Vec::new())
        .unwrap();
        snapshot.meta.ranking.scoring_digest[0] ^= 0xFF;
        let payload = postcard::to_allocvec(&snapshot).unwrap();
        let bytes = encode(&payload).unwrap();
        assert!(matches!(
            decode(&bytes),
            LoadOutcome::NeedsRebuild(RebuildReason::RankingProfileMismatch { .. })
        ));
    }
    #[test]
    fn store_round_trips_empty_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let (snapshot, _) = crate::index::SnapshotBuilder::new(crate::index::SnapshotMeta::new(
            None, true, [0; 32],
        ))
        .build(Vec::new())
        .unwrap();
        let store = SnapshotStore::new(directory.path());
        store.write(&snapshot).unwrap();
        let loaded = store.load().unwrap();
        match loaded {
            LoadOutcome::Ready(value) => assert_eq!(*value, snapshot),
            other => panic!("unexpected load outcome: {other:?}"),
        }
    }
}

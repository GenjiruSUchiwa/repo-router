//! The one content-identity value shared by indexing and source verification.
//!
//! It lives in `rr-core` so both the map pipeline (which produces it) and
//! verification (which compares it against the snapshot) name the same type.
//! `rr-git` owns every rule for *producing* one; nothing else may pair bytes
//! with an OID that could disagree with them.

use crate::index::ContentRepresentation;
use crate::oid::Oid;

/// Exact content bytes and the single OID that names them.
#[derive(Debug, Clone)]
pub struct AcquiredContent {
    /// Object identifier of `bytes` (Git blob OID, or local content hash).
    pub oid: Oid,
    /// Whether `oid` is a Git-canonical or raw local identity.
    pub representation: ContentRepresentation,
    /// The exact bytes that should be parsed, cached, and served.
    pub bytes: Vec<u8>,
}

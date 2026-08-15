//! The text projection of a snapshot: `MAP.md` routers and `.rr/SYMBOLS.md`.
//!
//! # What this module is
//!
//! One [`TextProjection`] is built from one frozen [`Snapshot`] and every text
//! artifact is rendered from it. Nothing here opens a source file, walks the
//! worktree, or derives one artifact from another. Existing files contribute
//! exactly one thing back: the validated contents of their `purpose` slot.
//!
//! That constraint is the whole point. A Markdown file that can disagree with
//! the index is a second truth, and a second truth is worse than no text at
//! all — a reader cannot tell which one is lying.
//!
//! # Reading order
//!
//! - [`digest`] — the one way structure becomes a stable name.
//! - [`encode`] — three encodings with three jobs, deliberately not
//!   interchangeable.
//! - [`model`] — snapshot records become canonical, ordered, hashed records.
//! - [`plan`] — the directory trie and the budgeted page planner.
//! - [`render`] — canonical bytes.
//! - [`parse`] — the strict, authoritative reader for those bytes.
//! - [`purpose`] — the only region a human owns.
//! - [`ignore`] — the managed block in `.rr/.gitignore`.
//! - [`validate`] — ownership, freshness, conflicts, and the map catalog.
//!
//! # Two deliberate readings of the specification
//!
//! **`## API` and `## Tests` are directory-local, not recursive.** Issue #11's
//! root-page sketch shows a nested file under the root map, but three of its
//! own contracts require one owning map per symbol: `.rr/SYMBOLS.md` has a
//! single `map` column, `MapCatalog::owner` returns a single identity, and
//! `api_hash` is a per-scope invalidation key. A recursive listing would put
//! every symbol in every ancestor map and leave all three undefined.
//!
//! **A record displays a qualified name and anchors a bare one.** Issue #11
//! says an API record uses "the qualified name when #6 has one", and separately
//! that a source anchor uses issue #7's grammar — and issue #7 builds anchors
//! from the *bare* name. Both are honored: `## API` and the `symbol` column of
//! `.rr/SYMBOLS.md` show the qualified spelling, which is the only one that
//! distinguishes two `new` methods in one file, while every anchor carries the
//! bare spelling `rr query` would print for the same location.
//!
//! **Link destinations are relative to the map that contains them; labels stay
//! repository-relative.** A destination is navigation and has to work when the
//! file is opened; a label is an identity, and the issue #7 anchor in a test
//! label is meant to be copied into `rr query`. At the root map the two
//! coincide, which is why the specification's example does not distinguish
//! them.

mod digest;
mod encode;
mod ignore;
mod model;
mod parse;
mod plan;
mod purpose;
mod render;
mod validate;

pub use digest::{ApiHash, Digest};
pub use ignore::{
    apply_managed_block, managed_ignore_block, IGNORE_BEGIN_MARKER, IGNORE_END_MARKER,
};
pub use model::{Fidelity, TextProjection};
pub use parse::{
    is_reserved_map_path, parse_map, parse_symbols, PageKind, ParsedApiRecord, ParsedMap,
    ParsedSymbolRecord, ParsedSymbols,
};
pub use purpose::{read_existing_purposes, ExistingPurposes};
pub use render::{ArtifactKind, RenderedArtifactSet, RenderedFile};
pub use validate::{
    validate_text_artifacts, validated_map_catalog, ArtifactState, Conflict, ConflictReason,
    MapCatalog, MapIdentity, TextValidation,
};

/// The on-disk format version of every artifact this module writes.
///
/// Bumped whenever parsing, canonicalization, escaping, hashing, page planning,
/// or ordering changes incompatibly — that is, whenever a file written by the
/// previous version would be misread rather than merely regenerated.
pub const TEXT_FORMAT_VERSION: u32 = 1;

/// The default `tokens` ceiling for one rendered page body.
pub const DEFAULT_MAP_BUDGET: u32 = 250;

/// The maximum size of a purpose slot's logical content.
///
/// Bytes, not characters, and deliberately so: a limit in characters would mean
/// a different limit on every platform's idea of a character, and this number
/// has to be checkable by anything that can read the file.
pub const PURPOSE_MAX_BYTES: usize = 160;

/// The committed router file name in every indexed directory.
pub const MAP_FILE_NAME: &str = "MAP.md";

/// The local, fully generated symbol index.
pub const SYMBOLS_PATH: &str = ".rr/SYMBOLS.md";

/// The `.rr` file whose managed block hides local artifacts.
pub const IGNORE_PATH: &str = ".rr/.gitignore";

/// The reserved prefix for generated overflow pages.
///
/// Any file matching this prefix in an indexed directory is rr-owned space,
/// whether or not the current plan uses that exact name.
pub const OVERFLOW_PREFIX: &str = "MAP.rr-";

/// The extension shared by every artifact this module writes.
const MARKDOWN_EXTENSION: &str = ".md";

/// One estimated token per this many UTF-8 body bytes.
const BYTES_PER_TOKEN: u32 = 4;

/// What went wrong while projecting, rendering, or reading text artifacts.
///
/// The variants are the categories a caller has to act on differently — a
/// grammar problem in a committed file means *stop and report*, while a
/// budget problem means *fix the configuration*. Free-form strings are avoided
/// so that a message change can never become a behavior change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("text artifact is not valid UTF-8")]
    NotUtf8,
    #[error("text artifact has invalid line endings: {reason}")]
    Newline { reason: &'static str },
    #[error("text artifact declares unsupported format {found}, expected {TEXT_FORMAT_VERSION}")]
    UnsupportedFormat { found: u32 },
    #[error("frontmatter is not the supported subset: {reason}")]
    Frontmatter { reason: &'static str },
    #[error("marker grammar is wrong: {reason}")]
    Marker { reason: &'static str },
    #[error("file contains Git conflict markers")]
    MergeConflict,
    #[error("generated record is not the supported grammar: {reason}")]
    Record { reason: &'static str },
    #[error("link destination is not canonical: {reason}")]
    Destination { reason: &'static str },
    #[error("purpose slot is not usable: {reason}")]
    Purpose { reason: &'static str },
    #[error("managed ignore block is not usable: {reason}")]
    ManagedIgnore { reason: &'static str },
    #[error("budget {budget} is unusable: {reason}")]
    Budget { budget: u32, reason: &'static str },
    #[error("two records claim the same identity: {reason}")]
    DuplicateRecord { reason: &'static str },
    #[error("stored generated_hash does not match the file's generated region")]
    GeneratedHashMismatch,
    #[error("artifacts disagree about index_hash; the generation is mixed")]
    IndexHashMismatch,
    #[error("staged bytes did not validate before replacement: {reason}")]
    Staging { reason: &'static str },
}

type TextResult<T> = std::result::Result<T, TextError>;

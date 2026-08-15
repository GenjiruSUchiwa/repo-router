//! The text projection of a snapshot: `MAP.md` routers and `.rr/SYMBOLS.md`.
//!
//! One [`TextProjection`] is built from one frozen [`Snapshot`] and every
//! artifact is rendered from it. Nothing here opens a source file, walks the
//! worktree, or derives one artifact from another. Existing files contribute
//! one thing back: the validated contents of their `purpose` slot.
//!
//! Submodules, in reading order: [`digest`], [`encode`], [`model`], [`plan`],
//! [`render`], [`parse`], [`purpose`], [`ignore`], [`validate`].
//!
//! # Where this reads issue #11 rather than quotes it
//!
//! - **`## API` and `## Tests` are directory-local, not recursive.** Three of
//!   the issue's own contracts need one owning map per symbol: the single `map`
//!   column, [`MapCatalog::owner`], and `api_hash` as a per-scope key.
//! - **A record displays the qualified name and anchors the bare one.** Issue
//!   #11 asks for the qualified name; issue #7's anchor grammar is built from
//!   the bare one. Both hold at once only by splitting them.
//! - **Destinations are relative to the containing map; labels stay
//!   repository-relative.** A destination has to resolve when the file is
//!   opened; a label is an identity meant to be pasted into `rr query`. They
//!   coincide at the root, which is why the issue's example does not separate
//!   them.

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

/// Whether a file of this name is one this module writes.
///
/// Discovery must skip these. Markdown is indexed, so an indexed map would
/// change the `index_hash` its own frontmatter carries, and no run would
/// converge.
#[must_use]
pub fn is_reserved_artifact_name(name: &str) -> bool {
    name == MAP_FILE_NAME
        || (name.starts_with(OVERFLOW_PREFIX) && name.ends_with(MARKDOWN_EXTENSION))
}

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

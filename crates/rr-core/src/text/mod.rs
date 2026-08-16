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
    stage_text_artifacts, validate_text_artifacts, validated_map_catalog, ArtifactState, Conflict,
    ConflictReason, MapCatalog, MapIdentity, StagedText, TextValidation,
};

/// The on-disk format version of every artifact this module writes.
///
/// Bumped whenever parsing, canonicalization, escaping, hashing, page planning,
/// or ordering changes incompatibly — that is, whenever a file written by the
/// previous version would be misread rather than merely regenerated.
///
/// Deliberately *not* bumped by #31, which widened the fact vocabulary and both
/// binary schemas. #31 taught the symbol-record parser two more visibility
/// labels, `protected` and `internal`, and that is a widening in one direction
/// only: this version reads everything the previous one wrote. The reverse —
/// an older binary meeting a file with a label it does not know — reaches
/// `.rr/SYMBOLS.md` alone, because `MAP.md` carries no visibility label, and
/// `validate::repairs_in_place` rewrites an unparseable `SYMBOLS.md` rather than
/// reporting it. No extractor emits either label yet in any case; #33 is what
/// makes them reachable.
///
/// Bumping instead would have cost what widening a *binary* schema does not.
/// This number is stamped into every committed `MAP.md` and folded into
/// `digest`, so a bump rewrites a file in every indexed directory of every
/// repository and moves every `index_hash` and `api_hash` — for a grammar
/// change no `MAP.md` can express. The day the rendered content of a map
/// changes, that bump is right and cheap by comparison.
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::facts::{Def, DefKind, Facts, ParseStatus, Span, TestSignals, Visibility};
    use crate::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
    use crate::lang::Lang;
    use crate::oid::Oid;
    use crate::path::RelPath;

    /// Every kind reaches a rendered map.
    ///
    /// Written against `DefKind::ALL` rather than against a fixture, because a
    /// fixture can only hold the kinds some language happens to produce today —
    /// and #33 is about to add kinds no Rust fixture will ever contain. What
    /// this rules out is a projection that groups by kind and drops the ones it
    /// does not recognise: a definition rr indexed but no reader can find is
    /// worse than one it never indexed, because nothing says it is missing.
    #[test]
    fn every_def_kind_renders_in_a_map() {
        let mut defs = Vec::new();
        for (index, kind) in DefKind::ALL.into_iter().enumerate() {
            let line = u32::try_from(index).unwrap() + 1;
            let start = u32::try_from(index).unwrap() * 32;
            let span = Span::new(start, start + 16, line, line).unwrap();
            let name = def_name(kind);
            defs.push(Def {
                signature: format!("{kind} {name}"),
                name,
                local_qualified: None,
                kind,
                visibility: Visibility::Public,
                span,
                signature_span: span,
                signature_idents: Vec::new(),
                body_idents: Vec::new(),
                doc_idents: Vec::new(),
                attribute_idents: Vec::new(),
                test_signals: TestSignals::default(),
            });
        }
        let facts = Facts::from_parts(defs, Vec::new(), Vec::new(), ParseStatus::Complete).unwrap();

        let input = FileInput {
            path: RelPath::new("src/kinds.rs").unwrap(),
            oid: Oid::from_raw(blake3::hash(b"kinds").as_bytes()).unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: facts.status(),
            facts,
        };
        let (snapshot, _) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
            .build(vec![input])
            .unwrap();

        let rendered = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET)
            .unwrap()
            .render(&ExistingPurposes::none())
            .unwrap();
        // Maps only, and every map in the generation: `.rr/SYMBOLS.md` lists
        // the same names, so counting it would let a kind pass this test while
        // being absent from the artifact the test is named after. Overflow
        // pages do count — twenty records do not fit a default budget, and a
        // record pushed onto a second page is still rendered.
        //
        // Parsed back into records rather than searched as text. These names
        // nest — `kind_const` is a substring of `kind_constructor`, `kind_trait`
        // of `kind_trait_method` — so a projection that dropped `Const` and
        // `Trait` would satisfy a `contains` on the rendered bytes and fail
        // nothing. Comparing records also makes the map's own grammar part of
        // what this test proves.
        let mut rendered_names: Vec<String> = Vec::new();
        for file in rendered.files() {
            if file.kind() == ArtifactKind::Symbols {
                continue;
            }
            let map = parse_map(file.bytes()).unwrap();
            // The record displays the qualified name; the kind is identified by
            // its last segment, which is the name the def was built with.
            rendered_names.extend(
                map.api()
                    .iter()
                    .map(|record| record.name.rsplit("::").next().unwrap_or("").to_owned()),
            );
        }

        // Before naming anything: every def produced exactly one record, so a
        // kind that vanished cannot be hidden by a kind that rendered twice,
        // and an empty parse cannot make the loop below vacuous.
        assert_eq!(rendered_names.len(), DefKind::ALL.len());

        for kind in DefKind::ALL {
            // `Vec::contains`, comparing whole names — not `str::contains`,
            // which is the substring search this assertion used to be and which
            // the nesting above defeats.
            assert!(
                rendered_names.contains(&def_name(kind)),
                "a {kind} definition reached the index and no map names it"
            );
        }
    }

    /// The definition name this test gives a kind, spelled once so the record it
    /// builds and the record it looks for cannot drift apart.
    fn def_name(kind: DefKind) -> String {
        format!("kind_{}", kind.as_str().replace('-', "_"))
    }
}

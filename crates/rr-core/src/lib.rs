#![deny(unsafe_code)]

/// Helpers the unit tests of more than one module need.
///
/// One copy and not one per module: a hand-written list of enum variants is
/// checked the same way wherever it lives, and two copies of the check would
/// drift the moment one of them was improved.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod test_support {
    /// Asserts that `T` has exactly `count` fieldless variants.
    ///
    /// Several enums in this crate are covered by hand-written lists that the
    /// compiler has no opinion about, so a variant added to one and forgotten
    /// in the other would leave its test quietly weaker than it reads. postcard
    /// writes a fieldless variant as its index, which makes the count
    /// checkable: the last index a list covers must decode, and the next one
    /// must not.
    pub(crate) fn assert_variant_count<T: serde::de::DeserializeOwned>(count: usize, listed: &str) {
        let last = u8::try_from(count - 1).unwrap();
        assert!(
            postcard::from_bytes::<T>(&[last]).is_ok(),
            "{listed} lists more variants than the enum has"
        );
        assert!(
            postcard::from_bytes::<T>(&[last + 1]).is_err(),
            "a variant was added to the enum and not to {listed}"
        );
    }
}

pub mod agent;
pub mod cache;
pub mod cancel;
pub mod content;
pub mod facts;
pub mod index;
pub mod json_contract;
pub mod lang;
pub mod lex;
pub mod oid;
pub mod parser;
pub mod path;
pub mod query;
pub mod ranking;
pub mod refresh;
pub mod render;
pub mod result;
pub mod snapshot;
pub mod text;
pub mod verify;
pub mod walk;
pub mod workspace;

pub use cache::{CacheKey, CacheOutcome, CacheStats, FactCache};
pub use cancel::CancelToken;
pub use content::AcquiredContent;
pub use facts::{
    Def, DefKind, DegradedReason, Facts, Import, ImportKind, LocalDefId, ParseStatus, Reference,
    ReferenceKind, Span, TestSignals, Visibility, FACT_SCHEMA_VERSION,
};
pub use lang::Lang;
pub use lex::TermId as LexTermId;
pub use lex::{
    append_source_terms, lexical_profile, query_terms, FieldTerm, InputKind, LexicalField,
    LexicalProfile, Lexicon, TermLookup, LEXICAL_VERSION,
};
pub use oid::{HashAlgo, Oid, OidError};
pub use refresh::{
    render_refresh_json, render_refresh_text, render_refresh_verbose, render_status_json,
    render_status_text, DiscoveryIdentity, FullReason, GitLabel, PlanDraft, RefreshCommand,
    RefreshError, RefreshMode, RefreshOutcome, RefreshPlan, RefreshReport, ReportDetail,
    ReportedMode, RunReport, SnapshotLabel, StatusReport, REFRESH_SCHEMA_VERSION,
    STATUS_SCHEMA_VERSION,
};

pub use parser::{extractor_version, tier, Tier};
pub use path::{RelPath, RelPathError};
pub use query::{
    finish_exact, parse_query, resolve_route_anchor, route_exact, route_query, ExactAtom,
    ExactAtomKind, ExactOutcome, ParsedQuery, QueryRequest,
};
pub use ranking::{
    decide, rank, route_lexical, CorpusStats, DecisionThresholds, FieldParams, FieldStats,
    MarginPpm, RankedSymbol, RankingError, RankingEvidence, RankingProfile, RankingScratch,
    RankingStamp, Score, CANDIDATE_LIMIT, DEFAULT_RANKING_PROFILE, RANKING_PROFILE_VERSION,
    RESULT_LIMIT,
};
pub use render::{
    decode_anchor, encode_anchor, render_json, render_json_explained, render_text,
    render_text_explained,
};
pub use result::{
    resolve_anchor, AnchorRef, Candidate, Confidence, LineRange, NoneReason, Pipeline, QueryResult,
    TargetId,
};
pub use verify::{
    finish_source, resolve_indexed_source, verify_source, AcquiredSource, ContentPathState,
    IndexedContentIdentity, IndexedSource, PendingPacket, PendingSource, Revalidation,
    SourcePacket, SourceResult, SourceStatus, MAX_SOURCE_BYTES, MAX_SOURCE_LINES,
    MAX_VERIFIED_CONTENT_BYTES, SOURCE_CONTEXT_AFTER, SOURCE_CONTEXT_BEFORE,
};
pub use walk::{discover, is_generated, SourceFile, WalkCfg, DEFAULT_EXCLUDES};

/// Core error types for `rr-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ignore error: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("invalid query: {reason}")]
    InvalidQuery { reason: &'static str },
    #[error("Invalid relative path: {0}")]
    InvalidRelPath(#[from] RelPathError),
    #[error("Cache I/O error at {path}: {source}")]
    CacheIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Cache serialization error: {0}")]
    CacheSerialization(#[from] postcard::Error),
    #[error("invalid lexical term: {reason}")]
    InvalidLexicon { reason: &'static str },
    #[error("lexical term id space exhausted")]
    TermIdExhausted,
    #[error("id space exhausted: {0}")]
    IdSpaceExhausted(&'static str),
    #[error("snapshot invariant violated: {reason}")]
    SnapshotInvariant { reason: &'static str },
    #[error("lexical ranking failed: {0}")]
    Ranking(#[from] ranking::RankingError),

    #[error("invalid span byte order: {start_byte}..{end_byte}")]
    InvalidSpanByteOrder { start_byte: u32, end_byte: u32 },
    #[error("invalid span line range: {start_line}..{end_line}")]
    InvalidSpanLineRange { start_line: u32, end_line: u32 },
    #[error("span {start_byte}..{end_byte} exceeds source length {len}")]
    SpanOutOfBounds {
        start_byte: u32,
        end_byte: u32,
        len: usize,
    },
    #[error("span offset {offset} is not a UTF-8 character boundary")]
    SpanNotCharBoundary { offset: u32 },
    #[error("span line metadata does not match source")]
    SpanLineMismatch,
    #[error("invalid facts: {reason}")]
    InvalidFacts { reason: &'static str },
    #[error("indexed content is not valid source: {reason}")]
    CorruptSource { reason: &'static str },
    #[error("invalid local definition id {id}; definition count is {definitions}")]
    InvalidLocalDefId { id: u32, definitions: usize },
    #[error("Tree-sitter language setup failed for {lang}: {message}")]
    ExtractorLanguage { lang: Lang, message: String },
    #[error("Tree-sitter query failed for {lang} at {row}:{column}: {message}")]
    ExtractorQuery {
        lang: Lang,
        row: usize,
        column: usize,
        message: String,
    },
    #[error("Tree-sitter query capture contract is incomplete: missing {capture}")]
    ExtractorQueryContract { capture: &'static str },
    #[error("Rust extraction invariant failed: {message}")]
    ExtractionInvariant { message: &'static str },
    #[error("text artifact: {0}")]
    Text(#[from] text::TextError),
}

/// A specialized [`Result`](std::result::Result) type for `rr-core` operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_result_type() {
        let ok_result: Result<&str> = Ok("success");
        assert!(ok_result.is_ok());
    }
}

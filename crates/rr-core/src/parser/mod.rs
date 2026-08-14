//! Language extractors and extractor versioning.

/// Bump on ANY change to the pinned Tree-sitter runtime/grammar version,
/// queries/rust.scm, capture interpretation, use-tree expansion,
/// qualification, test detection, fallback scanning, or ordering.
pub const EXTRACTOR_VERSION: u32 = 1;

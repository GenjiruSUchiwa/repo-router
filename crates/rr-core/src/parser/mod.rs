//! Language extractors and extractor versioning.

mod rust;

pub use rust::RustExtractor;

use crate::facts::{DegradedReason, Facts};

/// Bump on ANY change to the pinned Tree-sitter runtime/grammar version,
/// queries/rust.scm, capture interpretation, use-tree expansion,
/// qualification, test detection, fallback scanning, or ordering.
pub const EXTRACTOR_VERSION: u32 = 1;

const MAX_FALLBACK_BYTES: usize = 256 * 1024;
const MAX_FALLBACK_IDENTIFIERS: usize = 16 * 1024;

/// Scans ASCII identifiers from raw bytes without UTF-8 conversion.
///
/// Returns `(idents, scanned_bytes, truncated)`.
fn lexical_idents(content: &[u8]) -> (Vec<String>, u32, bool) {
    let byte_cap = content.len().min(MAX_FALLBACK_BYTES);
    let bytes = &content[..byte_cap];
    let mut idents = Vec::new();
    let mut i = 0usize;
    let mut truncated = content.len() > MAX_FALLBACK_BYTES;

    while i < bytes.len() {
        let b = bytes[i];
        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if idents.len() >= MAX_FALLBACK_IDENTIFIERS {
                truncated = true;
                let scanned = u32::try_from(start).unwrap_or(u32::MAX);
                return (idents, scanned, truncated);
            }
            let ident = bytes[start..i].iter().map(|&c| c as char).collect();
            idents.push(ident);
        } else {
            i += 1;
        }
    }

    let scanned = u32::try_from(byte_cap).unwrap_or(u32::MAX);
    (idents, scanned, truncated)
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn degraded_facts(content: &[u8], reason: DegradedReason) -> Facts {
    let (idents, scanned_bytes, truncated) = lexical_idents(content);
    Facts::degraded(idents, reason, scanned_bytes, truncated)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn lexical_idents_source_order_and_duplicates() {
        let (idents, scanned, truncated) = lexical_idents(b"foo bar foo");
        assert_eq!(idents, vec!["foo", "bar", "foo"]);
        assert_eq!(scanned, 11);
        assert!(!truncated);
    }

    #[test]
    fn lexical_idents_byte_cap_sets_truncated() {
        let mut content = vec![b'a'; MAX_FALLBACK_BYTES];
        content.push(b' ');
        content.extend_from_slice(b"tail");
        let (idents, scanned, truncated) = lexical_idents(&content);
        assert_eq!(idents, vec!["a".repeat(MAX_FALLBACK_BYTES)]);
        assert_eq!(scanned as usize, MAX_FALLBACK_BYTES);
        assert!(truncated);
    }

    #[test]
    fn lexical_idents_identifier_cap_sets_truncated() {
        let mut content = Vec::new();
        for i in 0..(MAX_FALLBACK_IDENTIFIERS + 2) {
            if i > 0 {
                content.push(b' ');
            }
            content.extend_from_slice(format!("id{i}").as_bytes());
        }
        let (idents, _scanned, truncated) = lexical_idents(&content);
        assert_eq!(idents.len(), MAX_FALLBACK_IDENTIFIERS);
        assert!(truncated);
    }

    #[test]
    fn lexical_idents_never_panics_on_binary() {
        let content = b"abc\xffdef\x00ghi";
        let (idents, scanned, truncated) = lexical_idents(content);
        assert_eq!(idents, vec!["abc", "def", "ghi"]);
        assert_eq!(scanned as usize, content.len());
        assert!(!truncated);
    }
}

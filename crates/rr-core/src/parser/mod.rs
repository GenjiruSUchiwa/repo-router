//! Language extractors and extractor versioning.

mod rust;

pub use rust::RustExtractor;

use std::collections::BTreeMap;

use crate::facts::{DegradedReason, Facts};
use crate::lang::Lang;
use crate::Result;

/// Bump on ANY change to the pinned Tree-sitter runtime/grammar version,
/// queries/rust.scm, capture interpretation, use-tree expansion,
/// qualification, test detection, fallback scanning, or ordering.
pub const EXTRACTOR_VERSION: u32 = 2;

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

/// Scans ASCII identifiers from already-validated UTF-8 text, uncapped.
///
/// The fallback path uses [`lexical_idents`] instead because it must cap work
/// on arbitrary bytes.
fn scan_idents(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if let Ok(ident) = std::str::from_utf8(&bytes[start..i]) {
                out.push(ident.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Builds degraded facts from a lexical scan, the fallback when no extractor
/// applies.
#[must_use]
pub fn degraded_facts(content: &[u8], reason: DegradedReason) -> Facts {
    let (idents, scanned_bytes, truncated) = lexical_idents(content);
    Facts::degraded(idents, reason, scanned_bytes, truncated)
}

/// Produces [`Facts`] from source bytes. One implementation per language.
pub trait Extractor: Send {
    /// The language this extractor parses.
    ///
    /// Exists so a registry entry can be checked against the language it is
    /// filed under. Without it, an extractor wired to the wrong language
    /// answers every call and reports nothing.
    fn lang(&self) -> Lang;

    /// Extracts facts from exactly these bytes.
    ///
    /// # Errors
    /// Returns extraction invariant and facts validation failures.
    fn extract(&mut self, content: &[u8]) -> Result<Facts>;
}

impl Extractor for RustExtractor {
    fn lang(&self) -> Lang {
        Lang::Rust
    }

    fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        RustExtractor::extract(self, content)
    }
}

type Builder = fn() -> Result<Box<dyn Extractor>, String>;

/// Every language that has an extractor, beside the only thing that may build
/// it.
///
/// One table rather than a `match` for the lookup and a list for the answer:
/// the pair is what makes them impossible to disagree. A language absent here
/// is not walked, so it can never reach a parser that cannot read it.
const EXTRACTORS: &[(Lang, Builder)] = &[(Lang::Rust, build_rust)];

fn build_rust() -> Result<Box<dyn Extractor>, String> {
    RustExtractor::new()
        .map(|extractor| Box::new(extractor) as Box<dyn Extractor>)
        .map_err(|error| error.to_string())
}

/// One lazily built extractor per supported language, keyed by [`Lang`].
///
/// Lazy because `Worker` — and therefore this registry — is constructed once
/// per rayon thread. Building every extractor eagerly would compile every
/// grammar's query on every thread, nearly all of them for languages that
/// thread never sees.
#[derive(Default)]
pub struct Registry {
    built: BTreeMap<Lang, Result<Box<dyn Extractor>, String>>,
}

impl Registry {
    /// Creates a registry with nothing built yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the extractor for `lang`, building it on first use.
    ///
    /// `None` means no extractor exists for the language, so the caller
    /// degrades. `Some(Err(_))` carries a construction failure so it surfaces
    /// on first use rather than at registry construction — which is why
    /// [`Registry::new`] cannot fail.
    pub fn for_lang(&mut self, lang: Lang) -> Option<Result<&mut dyn Extractor, String>> {
        // The builder travels with the language it belongs to, so the slot and
        // its contents cannot be mismatched by adding a row.
        let build = EXTRACTORS
            .iter()
            .find_map(|(candidate, build)| (*candidate == lang).then_some(*build))?;

        match self.built.entry(lang).or_insert_with(build) {
            Ok(extractor) => Some(Ok(extractor.as_mut())),
            Err(message) => Some(Err(message.clone())),
        }
    }

    /// The languages that have an extractor.
    ///
    /// The walk allowlist reads this, so a language rr cannot parse is never
    /// collected in the first place.
    #[must_use]
    pub fn supported() -> Vec<Lang> {
        EXTRACTORS.iter().map(|(lang, _)| *lang).collect()
    }
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
    #[test]
    fn supported_lists_only_rust() {
        assert_eq!(Registry::supported(), vec![Lang::Rust]);
    }

    #[test]
    fn every_supported_language_is_served_by_its_own_extractor() {
        for lang in Registry::supported() {
            let mut registry = Registry::new();
            let extractor = registry
                .for_lang(lang)
                .unwrap_or_else(|| panic!("{lang} is supported but has no extractor"))
                .unwrap_or_else(|message| panic!("{lang} failed to build: {message}"));

            // Not `is_some`: a builder filed under the wrong language answers
            // every call, so asking whether something replied proves nothing.
            // Only asking *what* replied catches a mismatched table row.
            assert_eq!(
                extractor.lang(),
                lang,
                "{lang} is served by a {} extractor",
                extractor.lang()
            );
        }
    }

    #[test]
    fn for_lang_returns_none_for_an_unsupported_language() {
        let mut registry = Registry::new();
        assert!(registry.for_lang(Lang::Python).is_none());
    }

    #[test]
    fn a_broken_extractor_surfaces_on_first_use_not_at_construction() {
        let mut registry = Registry::new();
        registry.built.insert(Lang::Rust, Err("broken".into()));
        assert!(matches!(
            registry.for_lang(Lang::Rust),
            Some(Err(message)) if message == "broken"
        ));
    }
}

pub mod split;
pub mod stem;
pub mod stop;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::{Error, Result};

pub const LEXICAL_VERSION: u32 = 1;

/// Version of the `unicode-normalization` crate that provides the NFC tables
/// used by the splitter. COUPLED to the `=` pin in the workspace `Cargo.toml`:
/// bumping that pin can change NFC output for affected codepoints, so this
/// constant must be bumped with it to invalidate stale snapshots.
pub const NORMALIZATION_CRATE_VERSION: (u8, u8, u8) = (0, 1, 24);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LexicalProfile {
    pub algorithm: u32,
    pub rust_unicode: (u8, u8, u8),
    pub normalization_crate: (u8, u8, u8),
}

#[must_use]
pub const fn lexical_profile() -> LexicalProfile {
    LexicalProfile {
        algorithm: LEXICAL_VERSION,
        rust_unicode: std::char::UNICODE_VERSION,
        normalization_crate: NORMALIZATION_CRATE_VERSION,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TermId(u32);

impl TermId {
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    #[must_use]
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LexicalField {
    Name,
    Qualified,
    Path,
    Signature,
    Body,
    Documentation,
    Attribute,
    Callee,
    Caller,
    Import,
}

impl LexicalField {
    /// Number of lexical fields; the canonical declaration order is [`Self::ALL`].
    pub const COUNT: usize = 10;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Name,
        Self::Qualified,
        Self::Path,
        Self::Signature,
        Self::Body,
        Self::Documentation,
        Self::Attribute,
        Self::Callee,
        Self::Caller,
        Self::Import,
    ];

    /// Position of this field in the canonical declaration order.
    ///
    /// This ordinal is the single source of truth for every per-field array in
    /// the crate: postings, field lengths, corpus statistics, and ranking
    /// parameters are all indexed by it.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Qualified => 1,
            Self::Path => 2,
            Self::Signature => 3,
            Self::Body => 4,
            Self::Documentation => 5,
            Self::Attribute => 6,
            Self::Callee => 7,
            Self::Caller => 8,
            Self::Import => 9,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Qualified => "qualified",
            Self::Path => "path",
            Self::Signature => "signature",
            Self::Body => "body",
            Self::Documentation => "documentation",
            Self::Attribute => "attribute",
            Self::Callee => "callee",
            Self::Caller => "caller",
            Self::Import => "import",
        }
    }
}

impl fmt::Display for LexicalField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Identifier,
    Qualified,
    Path,
    Prose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldTerm {
    pub field: LexicalField,
    pub term: TermId,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryTerms(SmallVec<[TermId; 8]>);

impl QueryTerms {
    #[must_use]
    pub fn as_slice(&self) -> &[TermId] {
        self.0.as_slice()
    }

    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = TermId> + '_ {
        self.0.iter().copied()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl IntoIterator for QueryTerms {
    type Item = TermId;
    type IntoIter = smallvec::IntoIter<[TermId; 8]>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a QueryTerms {
    type Item = &'a TermId;
    type IntoIter = std::slice::Iter<'a, TermId>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl AsRef<[TermId]> for QueryTerms {
    fn as_ref(&self) -> &[TermId] {
        self.as_slice()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(try_from = "Vec<String>")]
pub struct Lexicon {
    terms: Vec<Arc<str>>,
    indices: HashMap<Arc<str>, TermId>,
}

/// Looks up canonical terms without prescribing the backing index.
pub trait TermLookup {
    fn get(&self, canonical: &str) -> Option<TermId>;
}

impl TermLookup for Lexicon {
    fn get(&self, canonical: &str) -> Option<TermId> {
        Lexicon::get(self, canonical)
    }
}

impl Serialize for Lexicon {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.terms.iter().map(AsRef::<str>::as_ref))
    }
}

impl Lexicon {
    #[must_use]
    pub fn new() -> Self {
        Self {
            terms: Vec::new(),
            indices: HashMap::new(),
        }
    }

    #[must_use]
    pub fn get(&self, canonical: &str) -> Option<TermId> {
        self.indices.get(canonical).copied()
    }

    #[must_use]
    pub fn resolve(&self, id: TermId) -> Option<&str> {
        self.terms.get(id.index()).map(AsRef::as_ref)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.terms.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn terms(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.terms.iter().map(AsRef::as_ref)
    }

    pub(crate) fn intern(&mut self, canonical: &str) -> Result<TermId> {
        if let Some(&existing) = self.indices.get(canonical) {
            return Ok(existing);
        }

        let next_index = u32::try_from(self.terms.len()).map_err(|_| Error::TermIdExhausted)?;
        let id = TermId::from_index(next_index);
        let owned: Arc<str> = Arc::from(canonical);
        self.terms.push(Arc::clone(&owned));
        self.indices.insert(owned, id);
        Ok(id)
    }
}

impl TryFrom<Vec<String>> for Lexicon {
    type Error = Error;

    fn try_from(terms: Vec<String>) -> Result<Self> {
        if terms.len() > u32::MAX as usize {
            return Err(Error::InvalidLexicon {
                reason: "lexicon length exceeds u32::MAX",
            });
        }

        let mut shared_terms = Vec::with_capacity(terms.len());
        let mut indices = HashMap::with_capacity(terms.len());
        for (idx, term) in terms.into_iter().enumerate() {
            if term.is_empty() {
                return Err(Error::InvalidLexicon {
                    reason: "lexicon term cannot be empty",
                });
            }
            if !split::is_canonical_term(&term) {
                return Err(Error::InvalidLexicon {
                    reason: "lexicon term is not canonical",
                });
            }
            let next_index = u32::try_from(idx).map_err(|_| Error::InvalidLexicon {
                reason: "lexicon index exceeds u32::MAX",
            })?;
            let id = TermId::from_index(next_index);
            let shared: Arc<str> = Arc::from(term);
            shared_terms.push(Arc::clone(&shared));
            if indices.insert(shared, id).is_some() {
                return Err(Error::InvalidLexicon {
                    reason: "duplicate lexicon term",
                });
            }
        }

        Ok(Self {
            terms: shared_terms,
            indices,
        })
    }
}

/// Appends canonical lexical terms extracted from source facts to the output buffer.
///
/// # Errors
/// Returns an error if interning a term exceeds `TermId` capacity.
pub fn append_source_terms(
    field: LexicalField,
    kind: InputKind,
    input: &str,
    lexicon: &mut Lexicon,
    out: &mut SmallVec<[FieldTerm; 32]>,
) -> Result<()> {
    split::for_each_lexeme(input, |lexeme| {
        if (kind == InputKind::Prose || field == LexicalField::Documentation)
            && stop::is_stop_word(lexeme)
        {
            return Ok(());
        }
        let term = lexicon.intern(lexeme)?;
        out.push(FieldTerm { field, term });
        Ok(())
    })
}

#[must_use]
pub fn query_terms<L: TermLookup + ?Sized>(query: &str, lookup: &L) -> QueryTerms {
    let mut out = SmallVec::<[TermId; 8]>::new();
    let _ = split::for_each_lexeme(query, |lexeme| {
        if stop::is_stop_word(lexeme) {
            return Ok(());
        }

        if let Some(term_id) = lookup.get(lexeme) {
            if !out.contains(&term_id) {
                out.push(term_id);
            }
        }

        if let Some(short_stem) = stem::stem_lookup(lexeme) {
            if let Some(short_id) = lookup.get(short_stem) {
                if !out.contains(&short_id) {
                    out.push(short_id);
                }
            }
        }

        Ok(())
    });

    QueryTerms(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_ordinals_match_declaration_order() {
        for (position, field) in LexicalField::ALL.into_iter().enumerate() {
            assert_eq!(field.index(), position, "{field} ordinal");
        }
        assert_eq!(LexicalField::ALL.len(), LexicalField::COUNT);
    }

    #[test]
    fn test_term_id_from_index() {
        let id = TermId::from_index(7);
        assert_eq!(id.index(), 7);
    }

    #[test]
    fn test_lexicon_intern_direct() {
        let mut lexicon = Lexicon::new();
        let id0 = lexicon.intern("first").unwrap();
        let id1 = lexicon.intern("second").unwrap();
        let id0_again = lexicon.intern("first").unwrap();

        assert_eq!(id0, id0_again);
        assert_ne!(id0, id1);
        assert_eq!(id0.index(), 0);
        assert_eq!(id1.index(), 1);
    }
}

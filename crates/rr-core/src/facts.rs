//! Validated, postcard-serializable extraction facts.
//!
//! Deserialization enforces structural span and collection invariants. It cannot
//! validate spans against source bytes that are not present in the payload;
//! consumers that slice source MUST call [`Span::validate_for`] first.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Bump on ANY serialized field/type/serde-name/invariant change in this module.
pub const FACT_SCHEMA_VERSION: u32 = 1;

/// A source range over one exact UTF-8 byte buffer.
///
/// Bytes are half-open: `[start_byte, end_byte)`.
/// Lines are one-based and inclusive.
///
/// Deserialization revalidates structural bounds via [`Span::new`]. It cannot
/// validate against source bytes that are not present in the payload; consumers
/// slicing source call [`Span::validate_for`] first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawSpan", into = "RawSpan")]
pub struct Span {
    start_byte: u32,
    end_byte: u32,
    start_line: u32,
    end_line: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RawSpan {
    start_byte: u32,
    end_byte: u32,
    start_line: u32,
    end_line: u32,
}

impl From<Span> for RawSpan {
    fn from(span: Span) -> Self {
        Self {
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            start_line: span.start_line,
            end_line: span.end_line,
        }
    }
}

impl TryFrom<RawSpan> for Span {
    type Error = Error;

    fn try_from(raw: RawSpan) -> Result<Self> {
        Self::new(
            raw.start_byte,
            raw.end_byte,
            raw.start_line,
            raw.end_line,
        )
    }
}

impl Span {
    /// Builds a span after enforcing structural byte and line invariants.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSpanByteOrder`] or [`Error::InvalidSpanLineRange`]
    /// when the structural invariants fail.
    pub fn new(
        start_byte: u32,
        end_byte: u32,
        start_line: u32,
        end_line: u32,
    ) -> Result<Self> {
        if start_byte > end_byte {
            return Err(Error::InvalidSpanByteOrder {
                start_byte,
                end_byte,
            });
        }
        if start_line < 1 || end_line < 1 || start_line > end_line {
            return Err(Error::InvalidSpanLineRange {
                start_line,
                end_line,
            });
        }
        Ok(Self {
            start_byte,
            end_byte,
            start_line,
            end_line,
        })
    }

    /// Inclusive start byte offset.
    #[must_use]
    pub const fn start_byte(self) -> u32 {
        self.start_byte
    }

    /// Exclusive end byte offset.
    #[must_use]
    pub const fn end_byte(self) -> u32 {
        self.end_byte
    }

    /// One-based inclusive start line.
    #[must_use]
    pub const fn start_line(self) -> u32 {
        self.start_line
    }

    /// One-based inclusive end line.
    #[must_use]
    pub const fn end_line(self) -> u32 {
        self.end_line
    }

    /// Number of bytes covered by the span.
    #[must_use]
    pub const fn byte_len(self) -> u32 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Returns true when the span covers zero bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start_byte == self.end_byte
    }

    /// Returns true when `other` is fully contained in this span.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.start_byte <= other.start_byte && other.end_byte <= self.end_byte
    }

    /// Revalidates bounds, UTF-8 boundaries, and stored line numbers against
    /// the exact source representation.
    ///
    /// # Errors
    /// Returns a span error when the span does not match `source`.
    pub fn validate_for(self, source: &str) -> Result<()> {
        let len = source.len();
        let end = self.end_byte as usize;
        let start = self.start_byte as usize;
        if end > len {
            return Err(Error::SpanOutOfBounds {
                start_byte: self.start_byte,
                end_byte: self.end_byte,
                len,
            });
        }
        if !source.is_char_boundary(start) {
            return Err(Error::SpanNotCharBoundary {
                offset: self.start_byte,
            });
        }
        if !source.is_char_boundary(end) {
            return Err(Error::SpanNotCharBoundary {
                offset: self.end_byte,
            });
        }

        let expected_start = line_of(source.as_bytes(), start);
        if expected_start != self.start_line {
            return Err(Error::SpanLineMismatch);
        }

        let expected_end = if self.is_empty() {
            self.start_line
        } else {
            line_of(source.as_bytes(), end - 1)
        };
        if expected_end != self.end_line {
            return Err(Error::SpanLineMismatch);
        }
        Ok(())
    }
}

fn line_of(bytes: &[u8], offset: usize) -> u32 {
    let mut line = 1_u32;
    for &b in bytes.iter().take(offset) {
        if b == b'\n' {
            line = line.saturating_add(1);
        }
    }
    line
}

/// Stable index into the sorted definition vector of a [`Facts`] value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalDefId(u32);

impl LocalDefId {
    /// Zero-based index of this definition.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Builds an id from a validated definition index.
    #[must_use]
    pub(crate) const fn from_index(index: u32) -> Self {
        Self(index)
    }
}

/// Kind of a named definition extracted from source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefKind {
    Function,
    Method,
    TraitMethod,
    Struct,
    Enum,
    Union,
    Trait,
    TypeAlias,
    AssociatedType,
    Const,
    Static,
    Module,
    Macro,
}

impl DefKind {
    /// Stable kebab-case identifier matching the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::TraitMethod => "trait-method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::TypeAlias => "type-alias",
            Self::AssociatedType => "associated-type",
            Self::Const => "const",
            Self::Static => "static",
            Self::Module => "module",
            Self::Macro => "macro",
        }
    }
}

impl fmt::Display for DefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Visibility modifier of a definition or import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    Private,
    Public,
    Crate,
    Restricted(String),
}

/// Test-related signals derived from content attributes and `cfg` ancestry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TestSignals {
    /// An attached outer attribute is a recognized test attribute.
    pub explicit_attribute: bool,
    /// The definition or a content ancestor carries a lexical `cfg(test)` signal.
    pub inside_cfg_test: bool,
}

impl TestSignals {
    /// Returns true when either test signal is set.
    #[must_use]
    pub const fn any(self) -> bool {
        self.explicit_attribute || self.inside_cfg_test
    }
}

/// Kind of an unresolved syntactic reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceKind {
    Call,
    MethodCall,
    MacroCall,
    Implementation,
}

/// Kind of an import clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportKind {
    Use,
    ExternCrate,
}

/// Reason a file could not produce structural facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DegradedReason {
    InvalidUtf8,
    ParserReturnedNone,
    SourceTooLarge,
    QueryMatchLimit,
}

/// Outcome of extracting facts from one source buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParseStatus {
    Complete,
    Recovered {
        error_nodes: u32,
        missing_nodes: u32,
    },
    Degraded {
        reason: DegradedReason,
        scanned_bytes: u32,
        truncated: bool,
    },
}

/// A named definition extracted from source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Def {
    /// Bare definition name.
    pub name: String,
    /// Qualification derivable from this file's syntax only. Never includes a filesystem path.
    pub local_qualified: Option<String>,
    /// Definition kind.
    pub kind: DefKind,
    /// Visibility modifier.
    pub visibility: Visibility,
    /// Whole item, expanded backward to attached outer attributes/doc comments.
    pub span: Span,
    /// From the same expanded start through the byte before a body, or the whole item
    /// for semicolon-only definitions.
    pub signature_span: Span,
    /// Source-order occurrences; duplicates retained; definition name excluded.
    pub signature_idents: Vec<String>,
    /// Source-order occurrences owned by this definition; duplicates retained;
    /// nested definitions and reference-name occurrences excluded.
    pub body_idents: Vec<String>,
    /// Identifiers from attached outer doc comments only.
    pub doc_idents: Vec<String>,
    /// Identifiers from attached outer attributes, including cfg feature names.
    pub attribute_idents: Vec<String>,
    /// Explicit and `cfg(test)` test signals.
    pub test_signals: TestSignals,
}

/// An unresolved syntactic reference observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// Terminal referenced name, e.g. `send`.
    pub name: String,
    /// Preserved syntactic path where present, e.g. `module::send`; None for `value.send()`.
    pub qualified: Option<String>,
    /// Reference kind.
    pub kind: ReferenceKind,
    /// Span of the referenced name/path, not the entire call arguments.
    pub span: Span,
    /// Smallest containing definition after definitions are sorted; None at file scope.
    pub owner: Option<LocalDefId>,
}

/// A normalized import leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// Import kind.
    pub kind: ImportKind,
    /// Canonical leaf path, e.g. `crate::a::b`.
    pub path: String,
    /// Alias when present.
    pub alias: Option<String>,
    /// Whether the declaration is public.
    pub is_public: bool,
    /// Whether the leaf is a glob (`*`).
    pub is_glob: bool,
    /// Span of the leaf clause (or `*`), not necessarily the complete declaration.
    pub span: Span,
    /// Supports block-local `use`; None for file/module scope.
    pub owner: Option<LocalDefId>,
}

/// Validated extraction facts for one source buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawFacts", into = "RawFacts")]
pub struct Facts {
    defs: Vec<Def>,
    references: Vec<Reference>,
    imports: Vec<Import>,
    lexical_idents: Vec<String>,
    status: ParseStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawFacts {
    defs: Vec<Def>,
    references: Vec<Reference>,
    imports: Vec<Import>,
    lexical_idents: Vec<String>,
    status: ParseStatus,
}

impl From<Facts> for RawFacts {
    fn from(facts: Facts) -> Self {
        Self {
            defs: facts.defs,
            references: facts.references,
            imports: facts.imports,
            lexical_idents: facts.lexical_idents,
            status: facts.status,
        }
    }
}

impl TryFrom<RawFacts> for Facts {
    type Error = Error;

    fn try_from(raw: RawFacts) -> Result<Self> {
        validate_facts(
            &raw.defs,
            &raw.references,
            &raw.imports,
            &raw.lexical_idents,
            raw.status,
        )?;
        Ok(Self {
            defs: raw.defs,
            references: raw.references,
            imports: raw.imports,
            lexical_idents: raw.lexical_idents,
            status: raw.status,
        })
    }
}

impl Facts {
    /// Sorted definitions.
    #[must_use]
    pub fn defs(&self) -> &[Def] {
        &self.defs
    }

    /// Sorted references.
    #[must_use]
    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    /// Sorted imports.
    #[must_use]
    pub fn imports(&self) -> &[Import] {
        &self.imports
    }

    /// Lexical identifiers retained only for degraded extraction.
    #[must_use]
    pub fn lexical_idents(&self) -> &[String] {
        &self.lexical_idents
    }

    /// Extraction status.
    #[must_use]
    pub const fn status(&self) -> ParseStatus {
        self.status
    }

    /// Looks up a definition by id.
    #[must_use]
    pub fn def(&self, id: LocalDefId) -> Option<&Def> {
        self.defs.get(id.index())
    }

    /// Iterates references owned by `id`.
    pub fn references_from(&self, id: LocalDefId) -> impl Iterator<Item = &Reference> {
        self.references
            .iter()
            .filter(move |reference| reference.owner == Some(id))
    }

    /// Builds validated facts from sorted structural collections.
    ///
    /// # Errors
    /// Returns [`Error::InvalidFacts`] or [`Error::InvalidLocalDefId`] when
    /// invariants fail.
    pub(crate) fn from_parts(
        defs: Vec<Def>,
        references: Vec<Reference>,
        imports: Vec<Import>,
        status: ParseStatus,
    ) -> Result<Self> {
        validate_facts(&defs, &references, &imports, &[], status)?;
        Ok(Self {
            defs,
            references,
            imports,
            lexical_idents: Vec::new(),
            status,
        })
    }

    /// Builds degraded facts from a lexical scan. Always valid by construction.
    #[must_use]
    pub(crate) fn degraded(
        lexical_idents: Vec<String>,
        reason: DegradedReason,
        scanned_bytes: u32,
        truncated: bool,
    ) -> Self {
        Self {
            defs: Vec::new(),
            references: Vec::new(),
            imports: Vec::new(),
            lexical_idents,
            status: ParseStatus::Degraded {
                reason,
                scanned_bytes,
                truncated,
            },
        }
    }
}

fn validate_facts(
    defs: &[Def],
    references: &[Reference],
    imports: &[Import],
    lexical_idents: &[String],
    status: ParseStatus,
) -> Result<()> {
    if !defs_sorted(defs) {
        return Err(Error::InvalidFacts {
            reason: "definitions are not sorted",
        });
    }
    if !references_sorted(references) {
        return Err(Error::InvalidFacts {
            reason: "references are not sorted",
        });
    }
    if !imports_sorted(imports) {
        return Err(Error::InvalidFacts {
            reason: "imports are not sorted",
        });
    }

    for def in defs {
        if !def.span.contains(def.signature_span) {
            return Err(Error::InvalidFacts {
                reason: "signature span is not contained by definition span",
            });
        }
    }

    match status {
        ParseStatus::Complete => {
            if !lexical_idents.is_empty() {
                return Err(Error::InvalidFacts {
                    reason: "complete facts must not carry lexical identifiers",
                });
            }
        }
        ParseStatus::Recovered {
            error_nodes,
            missing_nodes,
        } => {
            if !lexical_idents.is_empty() {
                return Err(Error::InvalidFacts {
                    reason: "recovered facts must not carry lexical identifiers",
                });
            }
            if error_nodes == 0 && missing_nodes == 0 {
                return Err(Error::InvalidFacts {
                    reason: "recovered status requires at least one diagnostic node",
                });
            }
        }
        ParseStatus::Degraded { .. } => {
            if !defs.is_empty() || !references.is_empty() || !imports.is_empty() {
                return Err(Error::InvalidFacts {
                    reason: "degraded facts must not carry structural facts",
                });
            }
        }
    }

    if matches!(status, ParseStatus::Degraded { .. }) {
        return Ok(());
    }

    for reference in references {
        validate_owner(reference.owner, reference.span, defs)?;
    }
    for import in imports {
        validate_owner(import.owner, import.span, defs)?;
    }
    Ok(())
}

fn validate_owner(owner: Option<LocalDefId>, span: Span, defs: &[Def]) -> Result<()> {
    let Some(id) = owner else {
        if nearest_owner(span, defs).is_some() {
            return Err(Error::InvalidFacts {
                reason: "owner is missing for a contained fact",
            });
        }
        return Ok(());
    };
    if id.index() >= defs.len() {
        return Err(Error::InvalidLocalDefId {
            id: id.0,
            definitions: defs.len(),
        });
    }
    let expected = nearest_owner(span, defs);
    if expected != Some(id) {
        return Err(Error::InvalidFacts {
            reason: "owner is not the nearest containing definition",
        });
    }
    if !defs[id.index()].span.contains(span) {
        return Err(Error::InvalidFacts {
            reason: "owned fact is outside the owner span",
        });
    }
    Ok(())
}

/// Chooses the contained definition with the smallest byte length; ties use the
/// lowest sorted [`LocalDefId`].
#[must_use]
pub(crate) fn nearest_owner(span: Span, defs: &[Def]) -> Option<LocalDefId> {
    let mut best: Option<(u32, LocalDefId)> = None;
    for (index, def) in defs.iter().enumerate() {
        if !def.span.contains(span) {
            continue;
        }
        let len = def.span.byte_len();
        let id = LocalDefId::from_index(u32::try_from(index).unwrap_or(u32::MAX));
        match best {
            None => best = Some((len, id)),
            Some((best_len, best_id)) => {
                if len < best_len || (len == best_len && id < best_id) {
                    best = Some((len, id));
                }
            }
        }
    }
    best.map(|(_, id)| id)
}

fn defs_sorted(defs: &[Def]) -> bool {
    defs.windows(2).all(|pair| {
        def_key(&pair[0]) <= def_key(&pair[1])
    })
}

fn def_key(def: &Def) -> (u32, u32, DefKind, &str) {
    (
        def.span.start_byte(),
        def.span.end_byte(),
        def.kind,
        def.name.as_str(),
    )
}

fn references_sorted(references: &[Reference]) -> bool {
    references.windows(2).all(|pair| {
        reference_key(&pair[0]) <= reference_key(&pair[1])
    })
}

fn reference_key(reference: &Reference) -> (u32, u32, ReferenceKind, &str) {
    (
        reference.span.start_byte(),
        reference.span.end_byte(),
        reference.kind,
        reference.name.as_str(),
    )
}

fn imports_sorted(imports: &[Import]) -> bool {
    imports.windows(2).all(|pair| {
        import_key(&pair[0]) <= import_key(&pair[1])
    })
}

fn import_key(import: &Import) -> (u32, u32, ImportKind, &str, Option<&str>) {
    (
        import.span.start_byte(),
        import.span.end_byte(),
        import.kind,
        import.path.as_str(),
        import.alias.as_deref(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cache::{CacheKey, CacheOutcome, FactCache};
    use crate::lang::Lang;
    use crate::oid::Oid;
    use tempfile::TempDir;

    fn span(start: u32, end: u32, start_line: u32, end_line: u32) -> Span {
        Span::new(start, end, start_line, end_line).unwrap()
    }

    fn sample_def(name: &str, start: u32, end: u32) -> Def {
        Def {
            name: name.to_string(),
            local_qualified: None,
            kind: DefKind::Function,
            visibility: Visibility::Private,
            span: span(start, end, 1, 1),
            signature_span: span(start, end, 1, 1),
            signature_idents: Vec::new(),
            body_idents: Vec::new(),
            doc_idents: Vec::new(),
            attribute_idents: Vec::new(),
            test_signals: TestSignals::default(),
        }
    }

    #[test]
    fn span_accepts_empty_and_non_empty() {
        let empty = Span::new(0, 0, 1, 1).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.byte_len(), 0);
        let non_empty = Span::new(0, 4, 1, 1).unwrap();
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.byte_len(), 4);
        assert!(non_empty.contains(empty));
    }

    #[test]
    fn span_rejects_byte_order() {
        let err = Span::new(10, 3, 1, 1).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidSpanByteOrder {
                start_byte: 10,
                end_byte: 3
            }
        ));
    }

    #[test]
    fn span_rejects_line_range() {
        assert!(matches!(
            Span::new(0, 1, 0, 1).unwrap_err(),
            Error::InvalidSpanLineRange {
                start_line: 0,
                end_line: 1
            }
        ));
        assert!(matches!(
            Span::new(0, 1, 3, 2).unwrap_err(),
            Error::InvalidSpanLineRange {
                start_line: 3,
                end_line: 2
            }
        ));
    }

    #[test]
    fn validate_for_rejects_out_of_bounds() {
        let s = span(0, 5, 1, 1);
        let err = s.validate_for("abc").unwrap_err();
        assert!(matches!(
            err,
            Error::SpanOutOfBounds {
                start_byte: 0,
                end_byte: 5,
                len: 3
            }
        ));
    }

    #[test]
    fn validate_for_rejects_mid_codepoint() {
        let source = "αβ";
        let err = Span::new(1, 2, 1, 1)
            .unwrap()
            .validate_for(source)
            .unwrap_err();
        assert!(matches!(err, Error::SpanNotCharBoundary { offset: 1 }));
    }

    #[test]
    fn validate_for_rejects_line_mismatch_including_after_newline() {
        let source = "a\nb";
        let bad_start = Span::new(2, 3, 1, 2).unwrap();
        assert!(matches!(
            bad_start.validate_for(source).unwrap_err(),
            Error::SpanLineMismatch
        ));
        let ends_after_newline = Span::new(0, 2, 1, 2).unwrap();
        assert!(matches!(
            ends_after_newline.validate_for(source).unwrap_err(),
            Error::SpanLineMismatch
        ));
        let ok = Span::new(0, 2, 1, 1).unwrap();
        ok.validate_for(source).unwrap();
        let second = Span::new(2, 3, 2, 2).unwrap();
        second.validate_for(source).unwrap();
    }

    #[test]
    fn serde_json_rejects_reversed_span() {
        let err = serde_json::from_str::<Span>(
            r#"{"start_byte":10,"end_byte":3,"start_line":1,"end_line":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid span byte order"));
    }

    #[test]
    fn def_kind_serde_matches_as_str_and_display() {
        let kinds = [
            DefKind::Function,
            DefKind::Method,
            DefKind::TraitMethod,
            DefKind::Struct,
            DefKind::Enum,
            DefKind::Union,
            DefKind::Trait,
            DefKind::TypeAlias,
            DefKind::AssociatedType,
            DefKind::Const,
            DefKind::Static,
            DefKind::Module,
            DefKind::Macro,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(kind.to_string(), kind.as_str());
            let back: DefKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn facts_rejects_unsorted_collections() {
        let defs = vec![sample_def("b", 10, 20), sample_def("a", 0, 5)];
        let err = Facts::from_parts(defs, Vec::new(), Vec::new(), ParseStatus::Complete).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "definitions are not sorted"
            }
        ));

        let refs = vec![
            Reference {
                name: "b".into(),
                qualified: None,
                kind: ReferenceKind::Call,
                span: span(10, 11, 1, 1),
                owner: None,
            },
            Reference {
                name: "a".into(),
                qualified: None,
                kind: ReferenceKind::Call,
                span: span(0, 1, 1, 1),
                owner: None,
            },
        ];
        let err = Facts::from_parts(Vec::new(), refs, Vec::new(), ParseStatus::Complete).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "references are not sorted"
            }
        ));

        let imports = vec![
            Import {
                kind: ImportKind::Use,
                path: "b".into(),
                alias: None,
                is_public: false,
                is_glob: false,
                span: span(5, 6, 1, 1),
                owner: None,
            },
            Import {
                kind: ImportKind::Use,
                path: "a".into(),
                alias: None,
                is_public: false,
                is_glob: false,
                span: span(0, 1, 1, 1),
                owner: None,
            },
        ];
        let err =
            Facts::from_parts(Vec::new(), Vec::new(), imports, ParseStatus::Complete).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "imports are not sorted"
            }
        ));
    }

    #[test]
    fn facts_rejects_owner_out_of_range() {
        let refs = vec![Reference {
            name: "x".into(),
            qualified: None,
            kind: ReferenceKind::Call,
            span: span(0, 1, 1, 1),
            owner: Some(LocalDefId::from_index(3)),
        }];
        let err = Facts::from_parts(Vec::new(), refs, Vec::new(), ParseStatus::Complete).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidLocalDefId {
                id: 3,
                definitions: 0
            }
        ));
    }

    #[test]
    fn facts_rejects_owned_fact_outside_owner_span() {
        let defs = vec![sample_def("outer", 0, 10)];
        let refs = vec![Reference {
            name: "x".into(),
            qualified: None,
            kind: ReferenceKind::Call,
            span: span(20, 21, 1, 1),
            owner: Some(LocalDefId::from_index(0)),
        }];
        let err = Facts::from_parts(defs, refs, Vec::new(), ParseStatus::Complete).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "owner is not the nearest containing definition"
            }
        ));
    }

    #[test]
    fn facts_rejects_recovered_with_zero_diagnostics() {
        let err = Facts::from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ParseStatus::Recovered {
                error_nodes: 0,
                missing_nodes: 0,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "recovered status requires at least one diagnostic node"
            }
        ));
    }

    #[test]
    fn facts_rejects_degraded_with_structural_facts() {
        let raw = RawFacts {
            defs: vec![sample_def("x", 0, 1)],
            references: Vec::new(),
            imports: Vec::new(),
            lexical_idents: Vec::new(),
            status: ParseStatus::Degraded {
                reason: DegradedReason::InvalidUtf8,
                scanned_bytes: 0,
                truncated: false,
            },
        };
        let err = Facts::try_from(raw).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidFacts {
                reason: "degraded facts must not carry structural facts"
            }
        ));
    }

    #[test]
    fn facts_postcard_roundtrip() {
        let defs = vec![sample_def("a", 0, 20), sample_def("b", 5, 10)];
        let refs = vec![Reference {
            name: "call".into(),
            qualified: Some("mod::call".into()),
            kind: ReferenceKind::Call,
            span: span(6, 10, 1, 1),
            owner: Some(LocalDefId::from_index(1)),
        }];
        let imports = vec![Import {
            kind: ImportKind::Use,
            path: "crate::a".into(),
            alias: Some("b".into()),
            is_public: true,
            is_glob: false,
            span: span(7, 8, 1, 1),
            owner: Some(LocalDefId::from_index(1)),
        }];
        let facts = Facts::from_parts(defs, refs, imports, ParseStatus::Complete).unwrap();
        let bytes = postcard::to_allocvec(&facts).unwrap();
        let back: Facts = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, facts);
        assert_eq!(facts.def(LocalDefId::from_index(1)).unwrap().name, "b");
        assert_eq!(facts.references_from(LocalDefId::from_index(1)).count(), 1);
    }

    #[test]
    fn fact_cache_treats_invariant_breaking_facts_as_corrupt() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex("95d09f2b10159347eece71399a7e2e907ea3df4f").unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let raw = RawFacts {
            defs: vec![sample_def("b", 10, 20), sample_def("a", 0, 5)],
            references: Vec::new(),
            imports: Vec::new(),
            lexical_idents: Vec::new(),
            status: ParseStatus::Complete,
        };
        let bytes = postcard::to_allocvec(&raw).unwrap();
        let path = temp
            .path()
            .join(".rr")
            .join("local")
            .join("facts")
            .join(oid.shard_prefix())
            .join(format!("{}-rust-1-1.bin", oid.to_hex()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();

        let outcome: CacheOutcome<Facts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Corrupt);
    }

    #[test]
    fn degraded_constructor_is_valid() {
        let facts = Facts::degraded(
            vec!["foo".into()],
            DegradedReason::InvalidUtf8,
            4,
            false,
        );
        assert!(matches!(
            facts.status(),
            ParseStatus::Degraded {
                reason: DegradedReason::InvalidUtf8,
                scanned_bytes: 4,
                truncated: false
            }
        ));
        assert_eq!(facts.lexical_idents(), &["foo".to_string()]);
        assert!(facts.defs().is_empty());
    }

    #[test]
    fn nearest_owner_prefers_smallest_span() {
        let defs = vec![sample_def("outer", 0, 100), sample_def("inner", 10, 30)];
        let id = nearest_owner(span(12, 15, 1, 1), &defs).unwrap();
        assert_eq!(id.index(), 1);
    }
}

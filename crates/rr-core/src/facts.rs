//! Validated, postcard-serializable extraction facts.
//!
//! Deserialization enforces structural span and collection invariants. It cannot
//! validate spans against source bytes that are not present in the payload;
//! consumers that slice source MUST call [`Span::validate_for`] first.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Bump on ANY serialized field/type/serde-name/invariant change in this module.
///
/// Version 2 added [`Def::signature`]. A projection that shows a signature has
/// to read it from facts somebody already extracted, because re-slicing the
/// span later would mean opening a file the index has already spoken for — and
/// two readings of the same declaration is exactly the second truth the text
/// artifacts exist to rule out.
///
/// Version 3 widened the vocabulary from Rust's to a general one: new
/// [`DefKind`], [`Visibility`], and [`ImportKind`] variants, a neutral
/// [`TestSignals`] field, and [`DegradedReason::NoExtractor`]. Every change is
/// additive, so a Rust file's facts are unchanged in *content* — but postcard
/// writes enum variants positionally and structs field by field, so a version-2
/// payload decodes its successor's `TestSignals` out of alignment. The version
/// in the cache file name is what keeps the two apart: a stale entry is never
/// found, never misread, and simply reparsed.
/// Version 4 adds [`ParseStatus::Tags`], a validated tier-2 result produced
/// from a grammar's `tags.scm`. The status is serialized positionally, so the
/// new variant is appended rather than inserted between existing variants.
/// Version 5 adds [`Import::name`], the leaf a specifier-based language spells
/// in a slot separate from its source. Postcard writes structs field by field,
/// so every field after `path` shifts and a version-4 payload would decode its
/// successor's `alias` out of `name`'s bytes. The version in the cache file
/// name keeps the two apart: a stale entry is never found, never misread, and
/// simply reparsed.
pub const FACT_SCHEMA_VERSION: u32 = 5;

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
        Self::new(raw.start_byte, raw.end_byte, raw.start_line, raw.end_line)
    }
}

impl Span {
    /// Builds a span after enforcing structural byte and line invariants.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSpanByteOrder`] or [`Error::InvalidSpanLineRange`]
    /// when the structural invariants fail.
    pub fn new(start_byte: u32, end_byte: u32, start_line: u32, end_line: u32) -> Result<Self> {
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
///
/// The vocabulary is general, not Rust's: a language keeps the variants that
/// name its own constructs and ignores the rest. Rust never produces a `Class`;
/// TypeScript never produces a `Trait`. Nothing here is a lowest common
/// denominator, because a kind that had to fit every language would tell a
/// reader nothing about any of them.
///
/// **New variants are appended, never inserted.** `def_key` uses this
/// derived `Ord` as its final tiebreaker, so a variant slipped in among the
/// existing ones would reorder two definitions that share a span — a
/// content-identical repository whose facts no longer sort the same way.
///
/// A variant added here must also be added to [`DefKind::as_str`], which the
/// compiler enforces, and to [`DefKind::ALL`], which it does not.
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
    /// A nominal type with members. TypeScript and Python `class`.
    Class,
    /// A named contract carrying no implementation. TypeScript `interface`.
    ///
    /// Distinct from [`DefKind::Trait`] on purpose: a trait may carry default
    /// bodies and is implemented by name, an interface is satisfied
    /// structurally, and a reader looking for one should not be handed the
    /// other.
    Interface,
    /// A stored member of a type. A TypeScript class field, a Python class
    /// attribute.
    Field,
    /// A member reached like a field and computed like a method. A TypeScript
    /// getter or setter, a Python `@property`.
    Property,
    /// The member that builds an instance. A TypeScript `constructor`.
    Constructor,
    /// A named declaration scope that is not a file. A TypeScript `namespace`.
    ///
    /// [`DefKind::Module`] stays what Rust means by `mod`; a language that has
    /// both spellings can tell them apart.
    Namespace,
    /// A rebindable binding. A TypeScript `const`/`let`, a Python module-level
    /// assignment.
    ///
    /// [`DefKind::Const`] and [`DefKind::Static`] keep their Rust meanings,
    /// which say more than `Variable` does and are not generalised away.
    Variable,
}

impl DefKind {
    /// Every kind, in declaration order.
    ///
    /// Exists so that a projection or a test can prove it handles all of them
    /// rather than the handful its fixtures happen to contain.
    pub const ALL: [Self; 20] = [
        Self::Function,
        Self::Method,
        Self::TraitMethod,
        Self::Struct,
        Self::Enum,
        Self::Union,
        Self::Trait,
        Self::TypeAlias,
        Self::AssociatedType,
        Self::Const,
        Self::Static,
        Self::Module,
        Self::Macro,
        Self::Class,
        Self::Interface,
        Self::Field,
        Self::Property,
        Self::Constructor,
        Self::Namespace,
        Self::Variable,
    ];

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
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Field => "field",
            Self::Property => "property",
            Self::Constructor => "constructor",
            Self::Namespace => "namespace",
            Self::Variable => "variable",
        }
    }
}

impl fmt::Display for DefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Visibility modifier of a definition or import.
///
/// Rust's two spellings stay Rust's. A `pub(crate)` is not an `Internal` and a
/// `pub(in path)` is not a `Restricted` with the path thrown away: generalising
/// either one would lose the only detail that makes it worth recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    Private,
    Public,
    /// Visible to the declaring type and its subtypes. TypeScript `protected`.
    Protected,
    /// Visible within its unit by convention rather than by modifier. A Python
    /// `_name`, which PEP 8 makes a weak internal-use indicator — weaker than
    /// the `__name` that mangles and so reads as [`Visibility::Private`].
    Internal,
    /// Rust `pub(crate)`.
    Crate,
    /// Rust `pub(in path)`, carrying the path.
    Restricted(String),
}

/// Test-related signals derived from content attributes and enclosing scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TestSignals {
    /// An attached outer attribute is a recognized test attribute.
    ///
    /// Already language-neutral in substance: a Rust `#[test]`, a Python
    /// `@pytest.mark.parametrize`, and a Java `@Test` are the same claim
    /// written three ways.
    pub explicit_attribute: bool,
    /// The definition or a content ancestor carries a lexical `cfg(test)` signal.
    pub inside_cfg_test: bool,
    /// The definition sits inside something the language treats as a test
    /// scope, without saying so on the definition itself.
    ///
    /// Separate from [`TestSignals::inside_cfg_test`] rather than a
    /// reinterpretation of it: that field's name states a Rust mechanism and
    /// keeps meaning exactly that. A TypeScript `describe(…)` block and a
    /// Python `class Test…` are the same *kind* of signal, but they are not
    /// `#[cfg(test)]`, and a reader who greps for the Rust name must not find
    /// a TypeScript file.
    pub inside_test_scope: bool,
}

impl TestSignals {
    /// Returns true when any test signal is set.
    #[must_use]
    pub const fn any(self) -> bool {
        self.explicit_attribute || self.inside_cfg_test || self.inside_test_scope
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
///
/// Appended to rather than inserted into, for the reason [`DefKind`] gives:
/// `import_key` uses this derived `Ord` as a tiebreaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportKind {
    Use,
    ExternCrate,
    /// A whole module or its default binding. TypeScript `import x from "y"`,
    /// Python `import os`.
    Import,
    /// Named leaves selected out of a module. Python `from x import y`,
    /// TypeScript `import { y } from "x"` and `export { y } from "x"`.
    From,
    /// A module pulled in by call rather than by declaration. A `CommonJS`
    /// `require("y")`, including TypeScript's `import x = require("y")`.
    Require,
}

impl ImportKind {
    /// Whether [`Import::path`] is a path *this index's resolver* can follow to
    /// a definition it holds.
    ///
    /// Answered about the resolver, not about the language. `index::build`
    /// splits a path on `::` and rejoins it onto the importing file's module
    /// path, so Rust's `use` is the only kind that survives it. The other four
    /// are false for a reason no extractor work changes: their `path` is a
    /// specifier, not a path. `./Button`, `..` and `react` name a module the
    /// way a filesystem or a package resolver understands it, and turning one
    /// into a definition rr holds means building a module graph — tsconfig
    /// paths, extension resolution, `node_modules`, package layout. That graph
    /// is out of scope by decision, not by omission, so this is a settled
    /// answer rather than a placeholder: a row flips to `true` only alongside a
    /// resolver that can follow that language's specifiers, and the issue that
    /// builds one owns this predicate.
    ///
    /// Unresolved is a true statement about what rr knows; a resolution to the
    /// wrong definition is not — `react` imported from `app` and looked up as
    /// `app::react` would *find* an unrelated local symbol of that name.
    ///
    /// One predicate rather than a condition repeated at every resolution site:
    /// the two sites in `index::build` disagreeing about which kinds resolve is
    /// exactly how a symbol becomes reachable from one query and not another.
    #[must_use]
    pub const fn resolves_by_path(self) -> bool {
        match self {
            Self::Use => true,
            Self::ExternCrate | Self::Import | Self::From | Self::Require => false,
        }
    }
}

/// Reason a file could not produce structural facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DegradedReason {
    InvalidUtf8,
    /// The language's own parser was asked and returned nothing.
    ///
    /// A property of this file, reproducible from these bytes, and therefore
    /// worth caching. Not to be confused with [`DegradedReason::NoExtractor`],
    /// which the two shared until this vocabulary could tell them apart.
    ParserReturnedNone,
    SourceTooLarge,
    QueryMatchLimit,
    /// rr has no extractor for this language.
    ///
    /// A statement about rr at this moment, not about the file. The bytes did
    /// not change when the extractor landed, so facts carrying this reason must
    /// never be cached: the run that gained the extractor would be served the
    /// lexical-only entry the run before it wrote, and would have no way to
    /// know it had been.
    NoExtractor,
}

impl DegradedReason {
    /// Whether facts degraded for this reason are worth keeping for a later run.
    ///
    /// Every reason but one describes the file, and the file is what the cache
    /// is keyed on. [`DegradedReason::NoExtractor`] describes rr instead, and
    /// rr is not in the key.
    #[must_use]
    pub const fn is_cacheable(self) -> bool {
        match self {
            Self::InvalidUtf8
            | Self::ParserReturnedNone
            | Self::SourceTooLarge
            | Self::QueryMatchLimit => true,
            Self::NoExtractor => false,
        }
    }
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
    /// Tier-2 extraction from the grammar's tags query. Definitions, spans,
    /// signatures and identifier bags are real; visibility and test signals
    /// are judged from names and attributes by the language's conventions
    /// rather than from resolved semantics. `parse_errors` records tree
    /// errors.
    Tags {
        parse_errors: bool,
    },
}

impl ParseStatus {
    /// Whether facts with this status may be written to the fact cache.
    ///
    /// The question is answered from the status itself rather than from which
    /// branch produced it, so a caller cannot cache a `NoExtractor` degrade by
    /// forgetting which arm it came from.
    #[must_use]
    pub const fn is_cacheable(self) -> bool {
        match self {
            Self::Complete | Self::Recovered { .. } | Self::Tags { .. } => true,
            Self::Degraded { reason, .. } => reason.is_cacheable(),
        }
    }
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
    /// The declaration as one line, for readers that never see the source.
    ///
    /// Extracted here because this is the only place that holds both the span
    /// and the bytes it indexes. It deliberately starts at the item rather than
    /// at [`Def::span`]: attached documentation and attributes belong to the
    /// definition but not to the line that names it.
    pub signature: String,
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

/// Folds a declaration's source text into the single line a reader is shown.
///
/// Every maximal run of ASCII whitespace becomes one space and the ends are
/// trimmed, so a signature wrapped across five lines in the source and the same
/// signature written on one produce the same text. Anything else that would end
/// a line — a C0/C1 control character, or the two Unicode line separators a
/// Markdown renderer honours — is folded the same way rather than passed
/// through, because a record that spans two lines is a record no line-oriented
/// parser can read back.
///
/// The result can be empty only for input that is entirely whitespace; callers
/// that need a non-empty display substitute the definition's name.
#[must_use]
pub fn display_signature(raw: &str) -> String {
    let mut folded = String::with_capacity(raw.len());
    let mut pending_space = false;
    for character in raw.chars() {
        if breaks_a_line(character) {
            pending_space = !folded.is_empty();
            continue;
        }
        if pending_space {
            folded.push(' ');
            pending_space = false;
        }
        folded.push(character);
    }
    folded
}

/// Whether this character cannot survive in a one-line record.
const fn breaks_a_line(character: char) -> bool {
    character.is_ascii_whitespace()
        || character.is_control()
        || matches!(character, '\u{2028}' | '\u{2029}')
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
    /// What the declaration names as its source, in the language's own terms.
    ///
    /// Rust: a canonical module path whose last segment is the imported leaf,
    /// e.g. `crate::a::b`. A specifier-based language: the module specifier
    /// **exactly as written** — `./sibling`, `..`, `react` — unresolved,
    /// unnormalized, and with its escape sequences left undecoded. Resolving a
    /// specifier to a file needs a module graph rr does not build, and a `path`
    /// that looked canonical without being canonical is the failure this field
    /// is documented against. [`ImportKind::resolves_by_path`] is the predicate
    /// that says which of the two this is; no consumer may render `path` as a
    /// location rr resolved.
    pub path: String,
    /// The leaf selected out of `path`, when the language names it separately
    /// from the source it comes from.
    ///
    /// `Some` exactly when leaf and source occupy two syntactic slots:
    /// `from x import y` and `import { y } from "x"` both give `path = "x"`,
    /// `name = Some("y")`. `None` when the language spells one path that
    /// already ends in its leaf (Rust `use a::b::c`), and `None` when the
    /// declaration selects no leaf at all (`import os`, `import "./effect"`,
    /// `from x import *`).
    pub name: Option<String>,
    /// Alias when present.
    pub alias: Option<String>,
    /// Whether the declaration is public.
    pub is_public: bool,
    /// Whether the leaf brings in names it does not write down.
    ///
    /// `from x import *` and `export * from "m"` do. `import * as ns from "m"`
    /// does not: it binds one name, and is an aliased whole-module import.
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
        // Checked on the way in rather than on the way out: these facts also
        // arrive from the on-disk cache, where a file written by a build with a
        // different idea of canonical form would otherwise reach a renderer
        // that has no source left to re-derive it from.
        if def.signature.is_empty() {
            return Err(Error::InvalidFacts {
                reason: "definition signature is empty",
            });
        }
        if display_signature(&def.signature) != def.signature {
            return Err(Error::InvalidFacts {
                reason: "definition signature is not in canonical one-line form",
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
        ParseStatus::Tags { .. } => {
            if !lexical_idents.is_empty() {
                return Err(Error::InvalidFacts {
                    reason: "tags facts must not carry lexical identifiers",
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

    let owners = OwnerIndex::new(defs);
    for reference in references {
        validate_owner(
            reference.owner,
            owners.nearest(reference.span),
            reference.span,
            defs,
        )?;
    }
    for import in imports {
        validate_owner(
            import.owner,
            owners.nearest_import_owner(import.span),
            import.span,
            defs,
        )?;
    }
    Ok(())
}

fn validate_owner(
    owner: Option<LocalDefId>,
    expected: Option<LocalDefId>,
    span: Span,
    defs: &[Def],
) -> Result<()> {
    let Some(id) = owner else {
        if expected.is_some() {
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

/// Owner lookup over definitions sorted by `(start_byte, end_byte, ...)`.
///
/// Chooses the contained definition with the smallest byte length; ties use the
/// lowest sorted [`LocalDefId`]. Lookups binary-search the sorted starts and
/// walk back only while a containing span is still possible, so a lookup costs
/// O(log n + nesting depth) instead of a full scan.
pub(crate) struct OwnerIndex {
    spans: Vec<Span>,
    kinds: Vec<DefKind>,
    /// `prefix_max_end[i]` is the maximum `end_byte` over `spans[0..=i]`.
    prefix_max_end: Vec<u32>,
}

impl OwnerIndex {
    pub(crate) fn new(defs: &[Def]) -> Self {
        let spans: Vec<Span> = defs.iter().map(|def| def.span).collect();
        let kinds: Vec<DefKind> = defs.iter().map(|def| def.kind).collect();
        let mut prefix_max_end = Vec::with_capacity(spans.len());
        let mut max_end = 0_u32;
        for span in &spans {
            max_end = max_end.max(span.end_byte());
            prefix_max_end.push(max_end);
        }
        Self {
            spans,
            kinds,
            prefix_max_end,
        }
    }

    /// Smallest containing definition; None at file scope.
    pub(crate) fn nearest(&self, span: Span) -> Option<LocalDefId> {
        self.lookup(span, false)
    }

    /// Smallest containing non-module definition. Imports directly under a
    /// `mod` body sit at module scope and carry no owner; only block-local
    /// imports inside a definition do.
    pub(crate) fn nearest_import_owner(&self, span: Span) -> Option<LocalDefId> {
        self.lookup(span, true)
    }

    fn lookup(&self, span: Span, skip_modules: bool) -> Option<LocalDefId> {
        let first_after = self
            .spans
            .partition_point(|candidate| candidate.start_byte() <= span.start_byte());
        let mut best: Option<(u32, usize)> = None;
        for index in (0..first_after).rev() {
            if self.prefix_max_end[index] < span.end_byte() {
                break;
            }
            let candidate = self.spans[index];
            if !candidate.contains(span) {
                continue;
            }
            if skip_modules && self.kinds[index] == DefKind::Module {
                continue;
            }
            let len = candidate.byte_len();
            if best.is_none_or(|(best_len, _)| len <= best_len) {
                best = Some((len, index));
            }
        }
        best.map(|(_, index)| LocalDefId::from_index(u32::try_from(index).unwrap_or(u32::MAX)))
    }
}

fn defs_sorted(defs: &[Def]) -> bool {
    defs.windows(2)
        .all(|pair| def_key(&pair[0]) <= def_key(&pair[1]))
}

/// Canonical definition sort key; the extractor sorts with the same key the
/// validator checks.
pub(crate) fn def_key(def: &Def) -> (u32, u32, DefKind, &str) {
    (
        def.span.start_byte(),
        def.span.end_byte(),
        def.kind,
        def.name.as_str(),
    )
}

fn references_sorted(references: &[Reference]) -> bool {
    references
        .windows(2)
        .all(|pair| reference_key(&pair[0]) <= reference_key(&pair[1]))
}

/// Canonical reference sort key; the extractor sorts with the same key the
/// validator checks.
pub(crate) fn reference_key(reference: &Reference) -> (u32, u32, ReferenceKind, &str) {
    (
        reference.span.start_byte(),
        reference.span.end_byte(),
        reference.kind,
        reference.name.as_str(),
    )
}

fn imports_sorted(imports: &[Import]) -> bool {
    imports
        .windows(2)
        .all(|pair| import_key(&pair[0]) <= import_key(&pair[1]))
}

/// Canonical import sort key; the extractor sorts with the same key the
/// validator checks.
pub(crate) fn import_key(
    import: &Import,
) -> (u32, u32, ImportKind, &str, Option<&str>, Option<&str>) {
    (
        import.span.start_byte(),
        import.span.end_byte(),
        import.kind,
        import.path.as_str(),
        import.name.as_deref(),
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

    /// Asserts that `T` has exactly `count` fieldless variants.
    ///
    /// Three enums in this file are covered by hand-written lists that the
    /// compiler has no opinion about, so a variant added to one and forgotten in
    /// the other would leave its test quietly weaker than it reads. postcard
    /// writes a fieldless variant as its index, which makes the count checkable:
    /// the last index a list covers must decode, and the next one must not.
    fn assert_variant_count<T: serde::de::DeserializeOwned>(count: usize, listed: &str) {
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

    fn sample_def(name: &str, start: u32, end: u32) -> Def {
        Def {
            name: name.to_string(),
            local_qualified: None,
            kind: DefKind::Function,
            visibility: Visibility::Private,
            span: span(start, end, 1, 1),
            signature_span: span(start, end, 1, 1),
            signature: format!("fn {name}()"),
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
        for kind in DefKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(kind.to_string(), kind.as_str());
            let back: DefKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
            let bytes = postcard::to_allocvec(&kind).unwrap();
            let decoded: DefKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    /// `ALL` is the list every exhaustive walk trusts, and nothing but this
    /// test stops it from silently going stale.
    #[test]
    fn def_kind_all_names_each_kind_once() {
        let mut names: Vec<&str> = DefKind::ALL.iter().map(|kind| kind.as_str()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "a kind is listed in ALL twice");
        // Duplicate-freedom is only half of it: a kind missing from `ALL`
        // weakens every ALL-driven test, including the one asserting that every
        // kind reaches a map.
        assert_variant_count::<DefKind>(DefKind::ALL.len(), "DefKind::ALL");
    }

    /// The pre-#31 kinds keep their relative order, because [`def_key`] breaks
    /// span ties with it: an inserted variant would reorder two definitions
    /// that start and end at the same byte.
    #[test]
    fn def_kind_order_is_append_only() {
        let rust_kinds = [
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
        assert_eq!(DefKind::ALL[..rust_kinds.len()], rust_kinds);
        assert!(rust_kinds.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(DefKind::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn visibility_round_trips_including_the_widened_variants() {
        let all = [
            (Visibility::Private, "\"private\""),
            (Visibility::Public, "\"public\""),
            (Visibility::Protected, "\"protected\""),
            (Visibility::Internal, "\"internal\""),
            (Visibility::Crate, "\"crate\""),
            (
                Visibility::Restricted("super::inner".into()),
                "{\"restricted\":\"super::inner\"}",
            ),
        ];
        for (visibility, expected) in all {
            let json = serde_json::to_string(&visibility).unwrap();
            assert_eq!(json, expected);
            let back: Visibility = serde_json::from_str(&json).unwrap();
            assert_eq!(back, visibility);
            let bytes = postcard::to_allocvec(&visibility).unwrap();
            let decoded: Visibility = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(decoded, visibility);
        }
    }

    #[test]
    fn import_kind_round_trips_and_says_which_paths_resolve() {
        // Only `use` resolves, and deliberately: the resolver splits on `::`,
        // which no other kind's path uses. See `ImportKind::resolves_by_path`.
        let all = [
            (ImportKind::Use, "\"use\"", true),
            (ImportKind::ExternCrate, "\"extern-crate\"", false),
            (ImportKind::Import, "\"import\"", false),
            (ImportKind::From, "\"from\"", false),
            (ImportKind::Require, "\"require\"", false),
        ];
        for (kind, expected, resolves) in all {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, expected);
            let back: ImportKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
            assert_eq!(kind.resolves_by_path(), resolves, "{kind:?}");
        }
        assert!(ImportKind::Use < ImportKind::ExternCrate);
        assert!(ImportKind::ExternCrate < ImportKind::Import);
        assert_variant_count::<ImportKind>(all.len(), "the list above");
    }

    #[test]
    fn degraded_reason_round_trips_and_only_no_extractor_is_uncacheable() {
        let all = [
            (DegradedReason::InvalidUtf8, "\"invalid-utf8\"", true),
            (
                DegradedReason::ParserReturnedNone,
                "\"parser-returned-none\"",
                true,
            ),
            (DegradedReason::SourceTooLarge, "\"source-too-large\"", true),
            (
                DegradedReason::QueryMatchLimit,
                "\"query-match-limit\"",
                true,
            ),
            (DegradedReason::NoExtractor, "\"no-extractor\"", false),
        ];
        for (reason, expected, cacheable) in all {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, expected);
            let back: DegradedReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, reason);
            assert_eq!(reason.is_cacheable(), cacheable, "{reason:?}");
        }
        // A reason missing from the list is a reason nothing here asks
        // `is_cacheable` about, and the answer it would inherit by default is
        // the one that puts facts into the cache.
        assert_variant_count::<DegradedReason>(all.len(), "the list above");
    }

    /// The two reasons #31 separated must stay separated on the wire, since the
    /// whole point of adding one was that a reader can tell them apart.
    #[test]
    fn a_missing_extractor_and_a_failed_parse_are_different_facts() {
        let no_extractor = Facts::degraded(Vec::new(), DegradedReason::NoExtractor, 0, false);
        let parse_failure =
            Facts::degraded(Vec::new(), DegradedReason::ParserReturnedNone, 0, false);
        assert_ne!(no_extractor, parse_failure);
        assert!(!no_extractor.status().is_cacheable());
        assert!(parse_failure.status().is_cacheable());

        let bytes = postcard::to_allocvec(&no_extractor).unwrap();
        let back: Facts = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, no_extractor);
    }

    #[test]
    fn the_neutral_test_signal_counts_without_claiming_cfg_test() {
        let neutral = TestSignals {
            explicit_attribute: false,
            inside_cfg_test: false,
            inside_test_scope: true,
        };
        assert!(neutral.any());
        assert!(!neutral.inside_cfg_test, "a neutral scope is not cfg(test)");
        assert!(!TestSignals::default().any());

        let bytes = postcard::to_allocvec(&neutral).unwrap();
        let back: TestSignals = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, neutral);
    }

    /// Every widened variant reaches the on-disk format through a whole `Facts`
    /// value, not only on its own: a `Def` is what the cache actually stores.
    #[test]
    fn facts_round_trip_carries_every_widened_variant() {
        let mut defs = Vec::new();
        for (index, kind) in DefKind::ALL.into_iter().enumerate() {
            let start = u32::try_from(index).unwrap() * 10;
            let mut def = sample_def(kind.as_str(), start, start + 5);
            def.kind = kind;
            def.visibility = match index % 6 {
                0 => Visibility::Private,
                1 => Visibility::Public,
                2 => Visibility::Protected,
                3 => Visibility::Internal,
                4 => Visibility::Crate,
                _ => Visibility::Restricted("super".into()),
            };
            def.test_signals.inside_test_scope = index % 2 == 0;
            defs.push(def);
        }
        let imports = [
            ImportKind::Use,
            ImportKind::ExternCrate,
            ImportKind::Import,
            ImportKind::From,
            ImportKind::Require,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            let start = 1000 + u32::try_from(index).unwrap();
            Import {
                kind,
                path: format!("pkg{index}"),
                name: None,
                alias: None,
                is_public: false,
                is_glob: false,
                span: span(start, start + 1, 1, 1),
                owner: None,
            }
        })
        .collect();

        let facts = Facts::from_parts(defs, Vec::new(), imports, ParseStatus::Complete).unwrap();
        let bytes = postcard::to_allocvec(&facts).unwrap();
        let back: Facts = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, facts);
        assert_eq!(back.defs().len(), DefKind::ALL.len());
    }

    #[test]
    fn facts_rejects_unsorted_collections() {
        let defs = vec![sample_def("b", 10, 20), sample_def("a", 0, 5)];
        let err =
            Facts::from_parts(defs, Vec::new(), Vec::new(), ParseStatus::Complete).unwrap_err();
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
        let err =
            Facts::from_parts(Vec::new(), refs, Vec::new(), ParseStatus::Complete).unwrap_err();
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
                name: None,
                alias: None,
                is_public: false,
                is_glob: false,
                span: span(5, 6, 1, 1),
                owner: None,
            },
            Import {
                kind: ImportKind::Use,
                path: "a".into(),
                name: None,
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
        let err =
            Facts::from_parts(Vec::new(), refs, Vec::new(), ParseStatus::Complete).unwrap_err();
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
            name: None,
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
            .join(format!(
                "{}-rust-{}-{FACT_SCHEMA_VERSION}.bin",
                oid.to_hex(),
                crate::parser::extractor_version(crate::lang::Lang::Rust)
            ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();

        let outcome: CacheOutcome<Facts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Corrupt);
    }

    #[test]
    fn degraded_constructor_is_valid() {
        let facts = Facts::degraded(vec!["foo".into()], DegradedReason::InvalidUtf8, 4, false);
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
        let owners = OwnerIndex::new(&defs);
        let id = owners.nearest(span(12, 15, 1, 1)).unwrap();
        assert_eq!(id.index(), 1);
    }

    #[test]
    fn import_owner_skips_module_definitions() {
        let mut module = sample_def("holder", 0, 100);
        module.kind = DefKind::Module;
        let defs = vec![module, sample_def("inner", 10, 30)];
        let owners = OwnerIndex::new(&defs);
        assert_eq!(owners.nearest_import_owner(span(50, 55, 1, 1)), None);
        assert_eq!(
            owners
                .nearest_import_owner(span(12, 15, 1, 1))
                .unwrap()
                .index(),
            1
        );
        assert_eq!(owners.nearest(span(50, 55, 1, 1)).unwrap().index(), 0);
    }
}

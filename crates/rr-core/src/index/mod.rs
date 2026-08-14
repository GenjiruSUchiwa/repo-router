//! Deterministic repository index: IDs, immutable snapshot model, and validation.
//!
//! All identifier arenas use private-field `u32` newtypes with checked
//! constructors so a build can never silently truncate an id. [`Snapshot`]
//! serializes to a self-contained, byte-deterministic payload; [`Snapshot::validate`]
//! enforces every structural invariant that deserialization alone cannot prove.

use crate::facts::{DefKind, ImportKind, ParseStatus, ReferenceKind, Span, Visibility};
use crate::lang::Lang;
use crate::oid::Oid;
use crate::{Error, Result};

pub use crate::lex::TermId;

mod build;

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u32);

        impl $name {
            #[must_use]
            pub const fn index(self) -> usize {
                self.0 as usize
            }

            #[allow(dead_code)]
            pub(crate) const fn from_index(index: u32) -> Self {
                Self(index)
            }

            pub(crate) fn checked(index: usize) -> Result<Self> {
                let value =
                    u32::try_from(index).map_err(|_| Error::IdSpaceExhausted(stringify!($name)))?;
                Ok(Self(value))
            }
        }
    };
}

id_newtype!(FileId);
id_newtype!(SymbolId);
id_newtype!(StringId);

pub use build::{
    BuildReport, BuildStats, FileInput, IndexCounts, SnapshotBuilder, WorkerStats, BUILD_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentRepresentation {
    GitCanonical,
    RawNoGit,
}

/// Advisory build metadata and mode flags.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotMeta {
    /// Current `HEAD` commit OID, if the repository has one.
    pub repo_head_oid: Option<Oid>,
    /// True when the repository is not under Git control.
    pub no_git: bool,
    /// Lexical normalization profile used to build this snapshot.
    pub lexical_profile: crate::lex::LexicalProfile,
    /// Index-build semantic version ([`BUILD_VERSION`]) used to build this snapshot.
    pub build_version: u32,
}

/// One indexed source file and its arena slice.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileRecord {
    /// File id (equals its index in `files`).
    pub id: FileId,
    /// Path string in [`Snapshot::strings`].
    pub path: StringId,
    /// Detected language.
    pub language: Lang,
    /// Content identity of the exact parsed bytes.
    pub content_oid: Oid,
    /// Whether `content_oid` is a Git blob or local identity.
    pub representation: ContentRepresentation,
    /// Whether the file was classified as generated.
    pub generated: bool,
    /// Extraction outcome for this file.
    pub parse_status: ParseStatus,
    /// First symbol owned by this file (index into `symbols`).
    pub first_symbol: u32,
    /// Number of symbols owned by this file.
    pub symbol_count: u32,
    /// First reference owned by this file (index into `references`).
    pub first_reference: u32,
    /// Number of references owned by this file.
    pub reference_count: u32,
    /// First import owned by this file (index into `imports`).
    pub first_import: u32,
    /// Number of imports owned by this file.
    pub import_count: u32,
}

/// Per-field term frequencies for one symbol, consumed by later ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldLengths {
    /// `name` field term frequency.
    pub name: u32,
    /// `qualified` field term frequency.
    pub qualified: u32,
    /// `path` field term frequency.
    pub path: u32,
    /// `signature` field term frequency.
    pub signature: u32,
    /// `body` field term frequency.
    pub body: u32,
    /// `documentation` field term frequency.
    pub documentation: u32,
    /// `attribute` field term frequency.
    pub attribute: u32,
    /// `callee` field term frequency.
    pub callee: u32,
    /// `caller` field term frequency.
    pub caller: u32,
    /// `import` field term frequency.
    pub import: u32,
}

impl FieldLengths {
    /// All term frequencies zero.
    pub const ZERO: Self = Self {
        name: 0,
        qualified: 0,
        path: 0,
        signature: 0,
        body: 0,
        documentation: 0,
        attribute: 0,
        callee: 0,
        caller: 0,
        import: 0,
    };
}

/// One indexed definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolRecord {
    /// Symbol id (equals its index in `symbols`).
    pub id: SymbolId,
    /// Owning file.
    pub file: FileId,
    /// Bare definition name string.
    pub name: StringId,
    /// Repository-qualified name string.
    pub qualified_name: StringId,
    /// Definition kind.
    pub kind: DefKind,
    /// Visibility modifier.
    pub visibility: Visibility,
    /// Whole-item span.
    pub span: Span,
    /// Signature span.
    pub signature_span: Span,
    /// Test signals.
    pub test_signals: crate::facts::TestSignals,
    /// Per-field term frequencies.
    pub field_lengths: FieldLengths,
}

/// Local syntactic-resolution outcome for a reference or import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    /// Exactly one candidate symbol.
    Resolved(SymbolId),
    /// No candidate symbol.
    Unresolved,
    /// More than one candidate symbol.
    Ambiguous { candidates: u32 },
}

/// One unresolved syntactic reference observation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReferenceRecord {
    /// Owning file.
    pub file: FileId,
    /// Enclosing definition, if any.
    pub owner: Option<SymbolId>,
    /// Terminal referenced name string.
    pub name: StringId,
    /// Preserved syntactic qualification string, if any.
    pub qualified: Option<StringId>,
    /// Reference kind.
    pub kind: ReferenceKind,
    /// Span of the referenced name.
    pub span: Span,
    /// Conservative local resolution result.
    pub resolution: Resolution,
}

/// One normalized import leaf.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportRecord {
    /// Owning file.
    pub file: FileId,
    /// Enclosing definition, if any.
    pub owner: Option<SymbolId>,
    /// Canonical import path string.
    pub path: StringId,
    /// Import alias string, if any.
    pub alias: Option<StringId>,
    /// Import kind.
    pub kind: ImportKind,
    /// Whether the declaration is public.
    pub is_public: bool,
    /// Whether the leaf is a glob.
    pub is_glob: bool,
    /// Span of the leaf clause.
    pub span: Span,
    /// Conservative local resolution result.
    pub resolution: Resolution,
}

/// A canonical term in the shared string table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TermRecord {
    /// Canonical spelling in [`Snapshot::strings`].
    pub text: StringId,
}

/// An exact symbol route keyed by a name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExactRoute {
    /// Route key string.
    pub key: StringId,
    /// Strictly increasing, deduplicated symbols sharing the key.
    pub symbols: Vec<SymbolId>,
}

/// A `(symbol, term_frequency)` posting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolPosting {
    /// Target symbol.
    pub symbol: SymbolId,
    /// Occurrences of the posting-list term within this symbol.
    pub term_frequency: u32,
}

/// A `(file, term_frequency)` posting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilePosting {
    /// Target file.
    pub file: FileId,
    /// Occurrences of the posting-list term within this file's path.
    pub term_frequency: u32,
}

/// One term's postings for a single lexical field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PostingList {
    /// Term these postings belong to.
    pub term: TermId,
    /// Symbol postings, strictly increasing by symbol id.
    pub entries: Vec<SymbolPosting>,
}

/// One term's file-path postings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FilePostingList {
    /// Term these postings belong to.
    pub term: TermId,
    /// File postings, strictly increasing by file id.
    pub entries: Vec<FilePosting>,
}

/// Posting lists for every lexical field, in canonical declaration order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FieldPostings {
    /// `name` field posting lists.
    pub name: Vec<PostingList>,
    /// `qualified` field posting lists.
    pub qualified: Vec<PostingList>,
    /// `path` field posting lists.
    pub path: Vec<PostingList>,
    /// `signature` field posting lists.
    pub signature: Vec<PostingList>,
    /// `body` field posting lists.
    pub body: Vec<PostingList>,
    /// `documentation` field posting lists.
    pub documentation: Vec<PostingList>,
    /// `attribute` field posting lists.
    pub attribute: Vec<PostingList>,
    /// `callee` field posting lists.
    pub callee: Vec<PostingList>,
    /// `caller` field posting lists.
    pub caller: Vec<PostingList>,
    /// `import` field posting lists.
    pub import: Vec<PostingList>,
}

impl FieldPostings {
    /// All field posting vectors in canonical declaration order.
    pub const FIELDS: usize = 10;

    fn as_slice(&self) -> [&Vec<PostingList>; Self::FIELDS] {
        [
            &self.name,
            &self.qualified,
            &self.path,
            &self.signature,
            &self.body,
            &self.documentation,
            &self.attribute,
            &self.callee,
            &self.caller,
            &self.import,
        ]
    }
}

/// A self-contained, deterministic repository index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// Advisory build metadata.
    pub meta: SnapshotMeta,
    /// Shared string table for paths, names, qualifications, and term spellings.
    pub strings: Vec<String>,
    /// Canonical terms, each pointing into `strings`.
    pub terms: Vec<TermRecord>,
    /// Indexed files.
    pub files: Vec<FileRecord>,
    /// Indexed definitions.
    pub symbols: Vec<SymbolRecord>,
    /// Indexed references.
    pub references: Vec<ReferenceRecord>,
    /// Indexed imports.
    pub imports: Vec<ImportRecord>,
    /// Unqualified exact routes.
    pub exact_names: Vec<ExactRoute>,
    /// Repository-qualified exact routes.
    pub exact_qualified: Vec<ExactRoute>,
    /// Per-field lexical postings.
    pub postings: FieldPostings,
    /// File-path postings.
    pub file_path_postings: Vec<FilePostingList>,
}

impl Snapshot {
    /// Validates every arena id, range, ordering, and reference invariant.
    ///
    /// Deserialization enforces per-record structural bounds; this check proves
    /// cross-record consistency (ranges, monotonic ordering, duplicate freedom,
    /// owner/file agreement, and resolved-target bounds).
    ///
    /// # Errors
    /// Returns [`Error::SnapshotInvariant`] describing the first violation.
    pub fn validate(&self) -> Result<()> {
        validate::snapshot(self)
    }

    /// Builds a [`Lexicon`] from the terms and strings in this snapshot.
    ///
    /// # Errors
    /// Returns [`Error::InvalidLexicon`] if the terms table is malformed.
    pub fn lexicon(&self) -> Result<crate::lex::Lexicon> {
        let term_strings: Vec<String> = self
            .terms
            .iter()
            .map(|term| self.strings[term.text.index()].clone())
            .collect();
        crate::lex::Lexicon::try_from(term_strings)
    }
}

#[allow(
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::unnecessary_lazy_evaluations,
    clippy::trivially_copy_pass_by_ref
)]
mod validate {
    use super::*;

    pub(super) fn snapshot(s: &Snapshot) -> Result<()> {
        let n_files = s.files.len();
        let n_syms = s.symbols.len();
        let n_refs = s.references.len();
        check(
            s.meta.lexical_profile == crate::lex::lexical_profile(),
            "lexical profile mismatch",
        )?;
        check(
            s.meta.build_version == BUILD_VERSION,
            "build version mismatch",
        )?;
        let n_imps = s.imports.len();
        let n_terms = s.terms.len();
        let n_strings = s.strings.len();

        let mut seen_string: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(n_strings);
        for string in &s.strings {
            if string.is_empty() {
                return inv("empty string in table");
            }
            if !seen_string.insert(string.as_str()) {
                return inv("duplicate string in table");
            }
        }

        let mut seen_term: std::collections::HashSet<StringId> = std::collections::HashSet::new();
        for (i, term) in s.terms.iter().enumerate() {
            check(term.text.index() < n_strings, "term text out of range")?;
            if !seen_term.insert(term.text) {
                return inv("duplicate term text");
            }
            let _ = i;
        }
        let term_strings: Vec<String> = s
            .terms
            .iter()
            .map(|term| s.strings[term.text.index()].clone())
            .collect();
        if crate::lex::Lexicon::try_from(term_strings).is_err() {
            return inv("invalid canonical term");
        }

        let mut previous_path: Option<&[u8]> = None;
        for (i, file) in s.files.iter().enumerate() {
            check(file.id.index() == i, "file id mismatch")?;
            check(file.path.index() < n_strings, "file path out of range")?;
            let path = s.strings[file.path.index()].as_bytes();
            if let Some(previous) = previous_path {
                check(previous < path, "file paths not strictly sorted")?;
            }
            previous_path = Some(path);
            let end = file
                .first_symbol
                .checked_add(file.symbol_count)
                .ok_or_else(|| Error::SnapshotInvariant {
                    reason: "symbol range overflow",
                })?;
            check(end as usize <= n_syms, "symbol range exceeds symbols")?;
            let rend = file
                .first_reference
                .checked_add(file.reference_count)
                .ok_or_else(|| Error::SnapshotInvariant {
                    reason: "reference range overflow",
                })?;
            check(
                rend as usize <= n_refs,
                "reference range exceeds references",
            )?;
            let iend = file
                .first_import
                .checked_add(file.import_count)
                .ok_or_else(|| Error::SnapshotInvariant {
                    reason: "import range overflow",
                })?;
            check(iend as usize <= n_imps, "import range exceeds imports")?;
        }

        check_contiguous(s, |f| (f.first_symbol, f.symbol_count), n_syms)?;
        check_contiguous(s, |f| (f.first_reference, f.reference_count), n_refs)?;
        check_contiguous(s, |f| (f.first_import, f.import_count), n_imps)?;
        for file in &s.files {
            let symbol_start = file.first_symbol as usize;
            let symbol_end = symbol_start + file.symbol_count as usize;
            for symbol in &s.symbols[symbol_start..symbol_end] {
                check(
                    symbol.file == file.id,
                    "symbol file disagrees with file range",
                )?;
            }
            let reference_start = file.first_reference as usize;
            let reference_end = reference_start + file.reference_count as usize;
            for reference in &s.references[reference_start..reference_end] {
                check(
                    reference.file == file.id,
                    "reference file disagrees with file range",
                )?;
            }
            let import_start = file.first_import as usize;
            let import_end = import_start + file.import_count as usize;
            for import in &s.imports[import_start..import_end] {
                check(
                    import.file == file.id,
                    "import file disagrees with file range",
                )?;
            }
        }

        for (i, sym) in s.symbols.iter().enumerate() {
            check(sym.id.index() == i, "symbol id mismatch")?;
            check(sym.file.index() < n_files, "symbol file out of range")?;
            check(sym.name.index() < n_strings, "symbol name out of range")?;
            check(
                sym.qualified_name.index() < n_strings,
                "symbol qualified out of range",
            )?;
            check(
                sym.span.contains(sym.signature_span),
                "signature span outside symbol span",
            )?;
        }

        for r in &s.references {
            check(r.file.index() < n_files, "reference file out of range")?;
            check(r.name.index() < n_strings, "reference name out of range")?;
            if let Some(owner) = r.owner {
                check(owner.index() < n_syms, "reference owner out of range")?;
                check(
                    s.symbols[owner.index()].file == r.file,
                    "reference owner file disagrees",
                )?;
            }
            if let Some(q) = r.qualified {
                check(q.index() < n_strings, "reference qualified out of range")?;
            }
            check_resolution(&r.resolution, n_syms)?;
        }

        for im in &s.imports {
            check(im.file.index() < n_files, "import file out of range")?;
            check(im.path.index() < n_strings, "import path out of range")?;
            if let Some(owner) = im.owner {
                check(owner.index() < n_syms, "import owner out of range")?;
                check(
                    s.symbols[owner.index()].file == im.file,
                    "import owner file disagrees",
                )?;
            }
            if let Some(a) = im.alias {
                check(a.index() < n_strings, "import alias out of range")?;
            }
            check_resolution(&im.resolution, n_syms)?;
        }

        check_routes(s, &s.exact_names, n_strings, n_syms)?;
        check_routes(s, &s.exact_qualified, n_strings, n_syms)?;

        for list in s.postings.as_slice() {
            check_postings(s, list, n_terms, n_syms)?;
        }
        let mut previous_term_text: Option<&[u8]> = None;
        for list in &s.file_path_postings {
            check(
                list.term.index() < n_terms,
                "file posting term out of range",
            )?;
            let term_text = s.strings[s.terms[list.term.index()].text.index()].as_bytes();
            if let Some(previous) = previous_term_text {
                check(
                    previous < term_text,
                    "file postings not sorted by term text",
                )?;
            }
            previous_term_text = Some(term_text);
            let mut last_file: Option<FileId> = None;
            for posting in &list.entries {
                check(
                    posting.file.index() < n_files,
                    "file posting file out of range",
                )?;
                check(posting.term_frequency > 0, "zero term frequency")?;
                if let Some(previous) = last_file {
                    check(posting.file > previous, "file postings not increasing")?;
                }
                last_file = Some(posting.file);
            }
        }

        Ok(())
    }

    fn check_contiguous(
        s: &Snapshot,
        range: impl Fn(&FileRecord) -> (u32, u32),
        total: usize,
    ) -> Result<()> {
        let mut expected = 0u32;
        for file in &s.files {
            let (start, count) = range(file);
            check(start == expected, "non-contiguous file range")?;
            expected = start
                .checked_add(count)
                .ok_or_else(|| Error::SnapshotInvariant {
                    reason: "range overflow",
                })?;
        }
        check(expected as usize == total, "file ranges do not cover arena")?;
        Ok(())
    }

    fn check_resolution(r: &Resolution, n_syms: usize) -> Result<()> {
        match r {
            Resolution::Resolved(id) => check(id.index() < n_syms, "resolved target out of range"),
            Resolution::Unresolved => Ok(()),
            Resolution::Ambiguous { candidates } => {
                check(*candidates >= 2, "ambiguous with <2 candidates")
            }
        }
    }

    fn check_routes(
        s: &Snapshot,
        routes: &[ExactRoute],
        n_strings: usize,
        n_syms: usize,
    ) -> Result<()> {
        let mut last_key: Option<&str> = None;
        for route in routes {
            check(route.key.index() < n_strings, "route key out of range")?;
            let key = s.strings[route.key.index()].as_str();
            if let Some(prev) = last_key {
                check(prev.as_bytes() < key.as_bytes(), "routes not sorted by key")?;
            }
            last_key = Some(key);
            check(!route.symbols.is_empty(), "empty route")?;
            let mut last: Option<SymbolId> = None;
            for sym in &route.symbols {
                check(sym.index() < n_syms, "route symbol out of range")?;
                if let Some(prev) = last {
                    check(*sym > prev, "route symbols not increasing")?;
                }
                last = Some(*sym);
            }
        }
        Ok(())
    }

    fn check_postings(
        s: &Snapshot,
        list: &[PostingList],
        n_terms: usize,
        n_syms: usize,
    ) -> Result<()> {
        let mut previous_term_text: Option<&[u8]> = None;
        for posting in list {
            check(posting.term.index() < n_terms, "posting term out of range")?;
            let term_text = s.strings[s.terms[posting.term.index()].text.index()].as_bytes();
            if let Some(previous) = previous_term_text {
                check(previous < term_text, "postings not sorted by term text")?;
            }
            previous_term_text = Some(term_text);
            let mut last_symbol: Option<SymbolId> = None;
            for entry in &posting.entries {
                check(entry.symbol.index() < n_syms, "posting symbol out of range")?;
                check(entry.term_frequency > 0, "zero term frequency")?;
                if let Some(previous) = last_symbol {
                    check(entry.symbol > previous, "posting symbols not increasing")?;
                }
                last_symbol = Some(entry.symbol);
            }
        }
        Ok(())
    }

    fn check(cond: bool, reason: &'static str) -> Result<()> {
        if cond {
            Ok(())
        } else {
            inv(reason)
        }
    }

    fn inv(reason: &'static str) -> Result<()> {
        Err(Error::SnapshotInvariant { reason })
    }
}

//! Snapshot records become canonical, ordered, hashed text records.
//!
//! Everything in this file is a pure function of one [`Snapshot`]. No ordering
//! comes from a hash map, no string comes from a `Debug` rendering, and no
//! field is read twice from two places. That is what lets two machines render
//! the same repository into the same bytes.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::facts::{DefKind, ParseStatus, Visibility};
use crate::index::{FileRecord, Snapshot, SymbolId, SymbolRecord};
use crate::path::RelPath;

use super::digest::{ApiHash, Digest, HashStream};
use super::plan::{Records, ScopePlan};
use super::{encode, TextError, TextResult, BYTES_PER_TOKEN};

/// How much of a scope this projection claims to have understood.
///
/// A syntactic reading, never a compiler-semantic one. The field exists so a
/// reader cannot mistake "we parsed this" for "we resolved this", and so that a
/// file we failed to parse cannot masquerade as a file with no public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Fidelity {
    /// Every contributing file parsed completely.
    #[default]
    Syntax,
    /// Some contributing file parsed with recovered errors.
    Recovered,
    /// Some contributing file could not be parsed as source at all.
    Partial,
}

impl Fidelity {
    /// The published spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Recovered => "syntax-recovered",
            Self::Partial => "syntax-partial",
        }
    }

    /// Parses the published spelling, and only that.
    ///
    /// # Errors
    /// Returns [`TextError::Frontmatter`] for any other text.
    pub fn parse(text: &str) -> TextResult<Self> {
        match text {
            "syntax" => Ok(Self::Syntax),
            "syntax-recovered" => Ok(Self::Recovered),
            "syntax-partial" => Ok(Self::Partial),
            _ => Err(TextError::Frontmatter {
                reason: "fidelity is not one of syntax, syntax-recovered, syntax-partial",
            }),
        }
    }

    /// The worse of two claims.
    ///
    /// Worse wins, always. A scope that contains one unreadable file has not
    /// been fully read, however many of its siblings parsed cleanly.
    #[must_use]
    fn worst(self, other: Self) -> Self {
        self.max(other)
    }

    fn of(status: &ParseStatus) -> Self {
        match status {
            ParseStatus::Complete => Self::Syntax,
            ParseStatus::Recovered { .. } => Self::Recovered,
            ParseStatus::Degraded { .. } => Self::Partial,
        }
    }
}

/// The directory a map speaks for.
///
/// [`RelPath`] cannot hold the repository root — it rejects the empty path —
/// so the root is a variant here rather than a magic string threaded through
/// every function that touches a scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ScopePath {
    Root,
    Directory(RelPath),
}

impl ScopePath {
    /// The spelling used in `scope:` and in the page heading: `.` for the root.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Root => ".",
            Self::Directory(path) => path.as_str(),
        }
    }

    /// The repository-relative path of a file inside this scope.
    pub(crate) fn join(&self, file_name: &str) -> String {
        match self {
            Self::Root => file_name.to_owned(),
            Self::Directory(path) => format!("{}/{file_name}", path.as_str()),
        }
    }

    /// The map file that speaks for this scope.
    pub(crate) fn map_path(&self) -> String {
        self.join(super::MAP_FILE_NAME)
    }

    /// The number of path segments, used to publish deepest scopes first.
    pub(crate) fn depth(&self) -> usize {
        match self {
            Self::Root => 0,
            Self::Directory(path) => path.as_str().split('/').count(),
        }
    }

    /// Orders the root first, then by raw canonical path bytes.
    fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Root, Self::Root) => Ordering::Equal,
            (Self::Root, Self::Directory(_)) => Ordering::Less,
            (Self::Directory(_), Self::Root) => Ordering::Greater,
            (Self::Directory(left), Self::Directory(right)) => {
                left.as_str().as_bytes().cmp(right.as_str().as_bytes())
            }
        }
    }
}

/// How a definition's visibility is spelled.
///
/// Two spellings, on purpose. The label is what a reader sees and is one of
/// exactly three words. The key is what [`ApiHash`] consumes and keeps the
/// restriction path, so that `pub(in a)` becoming `pub(in b)` invalidates a
/// route even in the unlikely case that the display signature did not change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisibilityLabel {
    label: &'static str,
    key: String,
}

impl VisibilityLabel {
    /// The map-visible label, or `None` for a definition text never shows.
    fn of(visibility: &Visibility) -> Option<Self> {
        match visibility {
            Visibility::Private => None,
            Visibility::Public => Some(Self {
                label: "public",
                key: "public".to_owned(),
            }),
            Visibility::Crate => Some(Self {
                label: "crate",
                key: "crate".to_owned(),
            }),
            Visibility::Restricted(path) => Some(Self {
                label: "restricted",
                key: format!("restricted({path})"),
            }),
        }
    }
}

/// One definition as `## API` and `.rr/SYMBOLS.md` state it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiRecord {
    pub(crate) symbol: SymbolId,
    /// Repository-relative path of the declaring file.
    pub(crate) path: String,
    /// File name within the scope, which is what a link has to navigate to.
    pub(crate) file_name: String,
    /// Qualified name when the index has one, otherwise the bare name.
    ///
    /// What a reader sees. A method shown as a bare `new` would leave them
    /// guessing which of a file's five `impl` blocks it came from.
    pub(crate) name: String,
    /// The bare name, which is the only spelling an anchor may carry.
    ///
    /// Issue #7 builds its anchors from [`SymbolRecord::name`], so a map that
    /// linked to the qualified spelling would print an anchor `rr query` never
    /// prints — two spellings of one location, which is exactly the second
    /// truth this module exists to prevent.
    pub(crate) anchor_name: String,
    pub(crate) kind: DefKind,
    pub(crate) visibility: VisibilityLabel,
    pub(crate) signature: String,
    pub(crate) start_line: u32,
}

impl ApiRecord {
    /// Orders by name bytes, kind, signature bytes, then start line.
    ///
    /// Path is not part of this: records are grouped by file before they are
    /// ordered, so comparing paths here would only ever return `Equal`.
    fn order(&self, other: &Self) -> Ordering {
        self.name
            .as_bytes()
            .cmp(other.name.as_bytes())
            .then_with(|| self.kind.as_str().cmp(other.kind.as_str()))
            .then_with(|| self.signature.as_bytes().cmp(other.signature.as_bytes()))
            .then(self.start_line.cmp(&other.start_line))
    }

    /// Writes the fields `api_hash` covers.
    ///
    /// Body, documentation, attributes, start line, purpose, test records, and
    /// page packing are all absent by construction rather than by filtering:
    /// this function can only write what it is given, and it is given a record
    /// that never held them.
    fn write_api_hash(&self, stream: &mut HashStream) {
        stream.text(&self.path);
        stream.text(&self.name);
        // The anchor is what issue #12 will have stored against a route, and
        // an api_hash that did not cover it could leave a route pointing at a
        // location this projection no longer spells the same way.
        stream.text(&self.anchor_name);
        stream.text(self.kind.as_str());
        stream.text(&self.visibility.key);
        stream.text(&self.signature);
    }
}

/// One entry under `## Tests`.
///
/// A test file with no named test definition still gets one record, anchored at
/// line one, because a reader looking for the tests of a scope needs to find
/// the file even when this crate could not name anything inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestRecord {
    pub(crate) path: String,
    pub(crate) file_name: String,
    /// `None` for a test file with no named test definition.
    pub(crate) name: Option<String>,
    /// The bare name behind [`Self::name`], for the same reason as on
    /// [`ApiRecord`]: an anchor carries one spelling and it is issue #7's.
    pub(crate) anchor_name: Option<String>,
    pub(crate) kind: Option<DefKind>,
    pub(crate) signature: Option<String>,
    pub(crate) start_line: u32,
}

impl TestRecord {
    /// The issue #7 anchor this record displays and links to.
    pub(crate) fn anchor(&self) -> String {
        match &self.anchor_name {
            Some(name) => encode::source_anchor(&self.path, Some(name)),
            None => encode::file_anchor(&self.path),
        }
    }

    /// The scope-relative anchor a link in this scope's map navigates to.
    pub(crate) fn local_target(&self) -> (String, String) {
        let symbol = self.anchor_name.clone().unwrap_or_else(|| "L1".to_owned());
        (self.file_name.clone(), symbol)
    }

    fn order(&self, other: &Self) -> Ordering {
        self.path
            .as_bytes()
            .cmp(other.path.as_bytes())
            .then_with(|| {
                option_bytes(self.name.as_deref()).cmp(&option_bytes(other.name.as_deref()))
            })
            .then_with(|| kind_key(self.kind).cmp(kind_key(other.kind)))
            .then_with(|| {
                option_bytes(self.signature.as_deref())
                    .cmp(&option_bytes(other.signature.as_deref()))
            })
            .then(self.start_line.cmp(&other.start_line))
    }

    fn write_index_hash(&self, stream: &mut HashStream) {
        stream.text(&self.path);
        stream.text(self.name.as_deref().unwrap_or(""));
        stream.u8(u8::from(self.name.is_some()));
        stream.text(self.anchor_name.as_deref().unwrap_or(""));
        stream.text(kind_key(self.kind));
        stream.text(self.signature.as_deref().unwrap_or(""));
        stream.u32(self.start_line);
    }
}

/// A missing name sorts before every present one, and does so without an
/// allocation that would make the comparison depend on how it was built.
fn option_bytes(value: Option<&str>) -> Option<&[u8]> {
    value.map(str::as_bytes)
}

const fn kind_key(kind: Option<DefKind>) -> &'static str {
    match kind {
        Some(kind) => kind.as_str(),
        None => "",
    }
}

/// A link from a scope's router to the router of a directory beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildRecord {
    /// The child directory's final segment, which is what the label shows.
    pub(crate) name: String,
    /// The child's repository-relative directory path.
    pub(crate) path: String,
}

/// One directory and everything the text artifacts say about it.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) path: ScopePath,
    pub(crate) fidelity: Fidelity,
    pub(crate) api_hash: ApiHash,
    pub(crate) children: Vec<ChildRecord>,
    pub(crate) api: Vec<ApiRecord>,
    pub(crate) tests: Vec<TestRecord>,
    pub(crate) plan: ScopePlan,
}

/// One record of `.rr/SYMBOLS.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolLine {
    /// The symbol name, Markdown-destination encoded, as the file stores it.
    pub(crate) encoded_symbol: String,
    pub(crate) visibility: &'static str,
    /// The owning map path, Markdown-destination encoded.
    pub(crate) encoded_map: String,
    /// The issue #7 anchor, stored byte-for-byte so it can be pasted verbatim.
    pub(crate) anchor: String,
    pub(crate) start_line: u32,
    pub(crate) api_hash: ApiHash,
    /// Retained for ordering ties and for the duplicate check, not rendered.
    kind: DefKind,
}

impl SymbolLine {
    fn order(&self, other: &Self) -> Ordering {
        self.encoded_symbol
            .as_bytes()
            .cmp(other.encoded_symbol.as_bytes())
            .then_with(|| self.anchor.as_bytes().cmp(other.anchor.as_bytes()))
            .then_with(|| self.kind.as_str().cmp(other.kind.as_str()))
            .then(self.start_line.cmp(&other.start_line))
    }

    /// Everything a record states, for the duplicate check.
    ///
    /// Two records that agree on all of this are the same claim written twice,
    /// which the index should never have produced.
    fn identity(&self) -> (&str, &str, u32, &'static str) {
        (
            self.encoded_symbol.as_str(),
            self.anchor.as_str(),
            self.start_line,
            self.kind.as_str(),
        )
    }
}

/// One immutable text projection of one frozen snapshot.
///
/// Every field is private. A caller that could assemble its own scopes could
/// assemble a set whose `index_hash` does not describe it, and that hash is the
/// only thing standing between a torn multi-file write and a silent lie.
#[derive(Debug, Clone)]
pub struct TextProjection {
    pub(crate) budget: u32,
    pub(crate) fidelity: Fidelity,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) symbols: Vec<SymbolLine>,
    pub(crate) over_budget: Vec<String>,
    index_hash: Digest,
}

impl TextProjection {
    /// Projects one frozen snapshot at one resolved budget.
    ///
    /// # Errors
    /// Returns [`TextError::Budget`] when the budget cannot hold a router's
    /// fixed shell, and [`TextError::DuplicateRecord`] when the snapshot
    /// contains two identical symbol claims — an index invariant failure that
    /// this layer refuses to paper over by deduplicating.
    pub fn from_snapshot(snapshot: &Snapshot, budget: u32) -> crate::Result<Self> {
        Ok(Self::project(snapshot, budget)?)
    }

    /// The identity of the whole projection.
    ///
    /// Every rendered artifact carries this value, which is what makes a
    /// half-published generation detectable: two files that disagree here were
    /// written by two different runs.
    #[must_use]
    pub fn index_hash(&self) -> Digest {
        self.index_hash
    }

    /// The resolved page budget in tokens.
    #[must_use]
    pub const fn budget(&self) -> u32 {
        self.budget
    }

    /// Every directory scope, ordered root first then by canonical path bytes.
    pub(crate) fn scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// The repository-wide worst parse fidelity.
    #[must_use]
    pub const fn fidelity(&self) -> Fidelity {
        self.fidelity
    }

    /// Scopes whose plan contains one indivisible record larger than the budget.
    ///
    /// Reported rather than fixed: the alternative is truncating a signature,
    /// and a truncated signature is a wrong signature.
    pub fn over_budget_scopes(&self) -> impl Iterator<Item = &str> {
        self.over_budget.iter().map(String::as_str)
    }

    fn project(snapshot: &Snapshot, budget: u32) -> TextResult<Self> {
        let byte_budget = check_budget(budget)?;
        let mut directories = collect_directories(snapshot)?;
        link_children(&mut directories);

        let mut scopes: Vec<Scope> = Vec::with_capacity(directories.len());
        for (path, mut draft) in directories {
            draft.sort();
            let api_hash = api_hash_of(&draft.api);
            let plan = ScopePlan::build(&path, draft.records(), byte_budget);
            scopes.push(Scope {
                path,
                fidelity: draft.fidelity,
                api_hash,
                children: draft.children,
                api: draft.api,
                tests: draft.tests,
                plan,
            });
        }
        scopes.sort_by(|left, right| left.path.order(&right.path));

        let symbols = collect_symbol_lines(&scopes)?;
        let fidelity = scopes
            .iter()
            .fold(Fidelity::Syntax, |worst, scope| worst.worst(scope.fidelity));
        let over_budget = scopes
            .iter()
            .filter(|scope| scope.plan.is_over_budget())
            .map(|scope| scope.path.as_str().to_owned())
            .collect();
        let index_hash = index_hash_of(budget, fidelity, &scopes, &symbols);

        Ok(Self {
            budget,
            fidelity,
            scopes,
            symbols,
            over_budget,
            index_hash,
        })
    }
}

/// Hashes every snapshot-derived field either artifact exposes.
///
/// Purpose text, token estimates, and the generator version are absent: maps
/// differing only in human prose are the same projection, and treating them
/// otherwise would republish every file on every edit.
///
/// A free function over the parts rather than a method, so the hash cannot be
/// taken against a half-built projection.
fn index_hash_of(
    budget: u32,
    fidelity: Fidelity,
    scopes: &[Scope],
    symbols: &[SymbolLine],
) -> Digest {
    let mut stream = HashStream::new("index");
    stream.u32(budget);
    stream.text(fidelity.as_str());
    stream.count(scopes.len());
    for scope in scopes {
        stream.text(scope.path.as_str());
        stream.text(scope.fidelity.as_str());
        stream.digest(scope.api_hash.digest());
        stream.count(scope.children.len());
        for child in &scope.children {
            stream.text(&child.path);
        }
        stream.count(scope.api.len());
        for record in &scope.api {
            // Start line rides in the index hash but not in `api_hash`: moving
            // a definition changes what the map displays without changing the
            // API a route was learned against.
            record.write_api_hash(&mut stream);
            stream.u32(record.start_line);
        }
        stream.count(scope.tests.len());
        for record in &scope.tests {
            record.write_index_hash(&mut stream);
        }
        scope.plan.write_index_hash(&mut stream);
    }
    stream.count(symbols.len());
    for line in symbols {
        stream.text(&line.encoded_symbol);
        stream.text(line.visibility);
        stream.text(&line.encoded_map);
        stream.text(&line.anchor);
        stream.u32(line.start_line);
        stream.digest(line.api_hash.digest());
    }
    stream.finish()
}

/// The budget in body bytes, or the reason this budget cannot work.
///
/// A budget that cannot hold the fixed router shell plus the reserved purpose
/// bytes would put every scope over budget on the first run, which is a
/// configuration mistake worth naming rather than a warning to bury.
fn check_budget(budget: u32) -> TextResult<usize> {
    if budget == 0 {
        return Err(TextError::Budget {
            budget,
            reason: "a page budget of zero cannot hold a page",
        });
    }
    // No floor beyond that. A budget too small to hold even a page's fixed
    // shell is not an error but an over-budget scope, reported through the same
    // channel as one oversize signature — refusing here would be the only place
    // in this module where a budget problem stops a map from existing.
    Ok(budget as usize * BYTES_PER_TOKEN as usize)
}

/// Everything one directory contributes, before ordering and planning.
#[derive(Debug, Default)]
struct DirectoryDraft {
    fidelity: Fidelity,
    children: Vec<ChildRecord>,
    api: Vec<ApiRecord>,
    tests: Vec<TestRecord>,
}

impl DirectoryDraft {
    /// Puts every list in its one canonical order.
    ///
    /// API records group by file first because that is how they render — a
    /// `### path` heading followed by its definitions — and a page that split
    /// a file's definitions apart would repeat the heading for no reason.
    fn sort(&mut self) {
        self.children
            .sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
        self.api.sort_by(|left, right| {
            left.path
                .as_bytes()
                .cmp(right.path.as_bytes())
                .then_with(|| left.order(right))
        });
        self.tests.sort_by(TestRecord::order);
    }

    fn records(&self) -> Records<'_> {
        Records {
            children: &self.children,
            api: &self.api,
            tests: &self.tests,
        }
    }
}

impl Scope {
    /// The three record lists, bundled the way the planner and renderer read
    /// them.
    pub(crate) fn records(&self) -> Records<'_> {
        Records {
            children: &self.children,
            api: &self.api,
            tests: &self.tests,
        }
    }
}

/// Groups every indexed file into its directory, creating ancestors as needed.
///
/// A [`BTreeMap`] rather than a `HashMap` so that the traversal order is the
/// key order, not the seed order. Nothing downstream depends on that — every
/// list is sorted explicitly — but a container that can reorder is a place a
/// future change could leak nondeterminism into the bytes.
fn collect_directories(snapshot: &Snapshot) -> TextResult<BTreeMap<ScopePath, DirectoryDraft>> {
    let mut directories: BTreeMap<ScopePath, DirectoryDraft> = BTreeMap::new();
    directories.entry(ScopePath::Root).or_default();

    for file in &snapshot.files {
        let path = snapshot
            .file_path(file)
            .ok_or(TextError::Record {
                reason: "snapshot file record has no path string",
            })?
            .to_owned();
        let (scope, file_name) = split_directory(&path)?;
        // Fidelity is recursive, so every ancestor learns about this file even
        // though only its own directory lists it. Walking the chain here is
        // also what creates the routers for directories that hold nothing but
        // other directories.
        let file_fidelity = Fidelity::of(&file.parse_status);
        for ancestor in ancestors_of(&scope) {
            let draft = directories.entry(ancestor).or_default();
            draft.fidelity = draft.fidelity.worst(file_fidelity);
        }

        let draft = directories.entry(scope).or_default();
        collect_file(snapshot, file, &path, file_name, draft)?;
    }
    Ok(directories)
}

/// Splits a repository-relative file path into its scope and file name.
fn split_directory(path: &str) -> TextResult<(ScopePath, &str)> {
    match path.rsplit_once('/') {
        None => Ok((ScopePath::Root, path)),
        Some((directory, file_name)) => {
            let relative = RelPath::new(directory).map_err(|_| TextError::Record {
                reason: "snapshot file path has a directory the model cannot represent",
            })?;
            Ok((ScopePath::Directory(relative), file_name))
        }
    }
}

/// The scope itself and every scope above it, root last.
fn ancestors_of(scope: &ScopePath) -> Vec<ScopePath> {
    let mut chain = vec![scope.clone()];
    if let ScopePath::Directory(path) = scope {
        let mut current = path.as_str();
        while let Some((parent, _)) = current.rsplit_once('/') {
            if let Ok(relative) = RelPath::new(parent) {
                chain.push(ScopePath::Directory(relative));
            }
            current = parent;
        }
    }
    chain.push(ScopePath::Root);
    chain.dedup_by(|left, right| left == right);
    chain
}

/// Turns one file's symbols into API and test records for its directory.
fn collect_file(
    snapshot: &Snapshot,
    file: &FileRecord,
    path: &str,
    file_name: &str,
    draft: &mut DirectoryDraft,
) -> TextResult<()> {
    let first = file.first_symbol as usize;
    let end = first + file.symbol_count as usize;
    let symbols = snapshot.symbols.get(first..end).ok_or(TextError::Record {
        reason: "snapshot file record points outside the symbol arena",
    })?;

    let mut named_tests = 0_u32;
    for symbol in symbols {
        let name = snapshot
            .string(symbol.qualified_name)
            .ok_or(TextError::Record {
                reason: "snapshot symbol has no qualified name string",
            })?
            .to_owned();
        let anchor_name = snapshot
            .string(symbol.name)
            .ok_or(TextError::Record {
                reason: "snapshot symbol has no name string",
            })?
            .to_owned();
        let signature = snapshot
            .string(symbol.signature)
            .ok_or(TextError::Record {
                reason: "snapshot symbol has no signature string",
            })?
            .to_owned();
        if name.is_empty() || anchor_name.is_empty() || signature.is_empty() {
            return Err(TextError::Record {
                reason: "snapshot symbol has an empty name or signature",
            });
        }

        // A test is a test whatever its visibility: `## Tests` is a location
        // for a reader, not a claim about who may call it.
        if is_test(file, symbol) {
            named_tests += 1;
            draft.tests.push(TestRecord {
                path: path.to_owned(),
                file_name: file_name.to_owned(),
                name: Some(name),
                anchor_name: Some(anchor_name),
                kind: Some(symbol.kind),
                signature: Some(signature),
                start_line: symbol.span.start_line(),
            });
            continue;
        }
        let Some(visibility) = VisibilityLabel::of(&symbol.visibility) else {
            continue;
        };
        draft.api.push(ApiRecord {
            symbol: symbol.id,
            path: path.to_owned(),
            file_name: file_name.to_owned(),
            name,
            anchor_name,
            kind: symbol.kind,
            visibility,
            signature,
            start_line: symbol.span.start_line(),
        });
    }

    if file.is_test && named_tests == 0 {
        draft.tests.push(TestRecord {
            path: path.to_owned(),
            file_name: file_name.to_owned(),
            name: None,
            anchor_name: None,
            kind: None,
            signature: None,
            start_line: 1,
        });
    }
    Ok(())
}

/// Whether this definition belongs under `## Tests`.
///
/// One predicate, called from one place, so that a map and `.rr/SYMBOLS.md`
/// cannot classify the same definition differently.
fn is_test(file: &FileRecord, symbol: &SymbolRecord) -> bool {
    file.is_test || symbol.test_signals.any()
}

/// Gives every directory a link to each directory directly beneath it.
fn link_children(directories: &mut BTreeMap<ScopePath, DirectoryDraft>) {
    let paths: Vec<ScopePath> = directories.keys().cloned().collect();
    for path in paths {
        let ScopePath::Directory(relative) = &path else {
            continue;
        };
        let text = relative.as_str();
        let (parent, name) = match text.rsplit_once('/') {
            Some((parent, name)) => (
                RelPath::new(parent).map_or(ScopePath::Root, ScopePath::Directory),
                name,
            ),
            None => (ScopePath::Root, text),
        };
        if let Some(draft) = directories.get_mut(&parent) {
            draft.children.push(ChildRecord {
                name: name.to_owned(),
                path: text.to_owned(),
            });
        }
    }
}

/// The per-scope API identity issue #12 stores against a route.
fn api_hash_of(records: &[ApiRecord]) -> ApiHash {
    let mut stream = HashStream::new("api");
    stream.u32(u32::try_from(records.len()).unwrap_or(u32::MAX));
    for record in records {
        record.write_api_hash(&mut stream);
    }
    ApiHash::new(stream.finish())
}

/// Flattens every scope's API into the sorted `.rr/SYMBOLS.md` record list.
fn collect_symbol_lines(scopes: &[Scope]) -> TextResult<Vec<SymbolLine>> {
    let mut lines = Vec::new();
    for scope in scopes {
        let encoded_map = encode::destination(&scope.path.map_path(), None);
        for record in &scope.api {
            lines.push(SymbolLine {
                encoded_symbol: encode::destination(&record.name, None),
                visibility: record.visibility.label,
                encoded_map: encoded_map.clone(),
                anchor: encode::source_anchor(&record.path, Some(&record.anchor_name)),
                start_line: record.start_line,
                api_hash: scope.api_hash,
                kind: record.kind,
            });
        }
    }
    lines.sort_by(SymbolLine::order);
    // Sorted, so identical records are adjacent. Reported rather than
    // deduplicated: two identical claims mean the index built something twice,
    // and hiding that here would leave the bug to be found by whoever trusts
    // the symbol count.
    if lines
        .windows(2)
        .any(|pair| pair[0].identity() == pair[1].identity())
    {
        return Err(TextError::DuplicateRecord {
            reason: "two symbols share a name, anchor, kind, and start line",
        });
    }
    Ok(lines)
}

impl Ord for ScopePath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order(other)
    }
}

impl PartialOrd for ScopePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

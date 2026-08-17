//! Which definitions a change reaches, and the evidence for every claim.
//!
//! Structural, and from resolved index edges only. What the index could not
//! resolve is counted, never guessed: a wrong `affected` entry sends an agent to
//! edit code that never needed it, which is worse than an honest "unknown".
//!
//! # An edge is a resolution, not a name match
//!
//! One edge exists for exactly one reason: a [`ReferenceRecord`] or an
//! [`ImportRecord`] carries [`Resolution::Resolved`]. [`Resolution::Unresolved`]
//! and [`Resolution::Ambiguous`] are **counters** — see [`ResolutionCounts`] —
//! and never edges. That is not caution about a weak resolver, it is the whole
//! contract of the command: `index::build` makes every
//! [`ReferenceKind::MethodCall`] permanently unresolved because it has no
//! receiver type, and resolves an import only when
//! [`crate::facts::ImportKind::resolves_by_path`] holds, which is Rust's `use` and nothing
//! else. A wider edge set would be rr guessing, and a report that guesses is
//! indistinguishable from a report that knows.
//!
//! Five kinds of evidence are deliberately not traversed, each for a reason that
//! no amount of extractor work changes: unresolved and ambiguous observations
//! (above); method calls (no receiver type); macro expansion (a `MacroCall` is a
//! name, not a body); build-graph edges (in no record this crate holds); and
//! lexical or historical similarity, which is correlational and therefore
//! attaches to [`ImpactResultV1::tests`] alone, never to `affected`.
//!
//! # What the report is about
//!
//! Two endpoints, resolved once by the caller, and the definitions the deltas
//! between them touch. Those are the *seeds*: removed definitions on the base
//! side, plus added, modified and moved definitions on the target side. From
//! them, `affected` is the reverse closure — who points *at* a seed — and
//! `dependencies` is the forward closure, which is evidence rather than a claim
//! that a dependency must change. Both are bounded by [`DEFAULT_DEPTH`] and
//! [`MAX_DEPTH`], and `distance` is a minimum edge count.
//!
//! # Determinism
//!
//! No timestamp, no elapsed time, no absolute path, no colour, and no hash-map
//! order reaches any output: two runs over one pair of endpoints are
//! byte-identical, which is what makes a report diffable in a pull request.
//! Every collection is ordered by [`BTreeMap`] or [`BTreeSet`], and
//! [`ImpactResultV1::canonicalize`] settles the order both renderers see before
//! either one runs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub mod render;

pub use render::{render_impact_json, render_impact_text};

use crate::check::{DiagnosticV1, Severity};
use crate::facts::{DefKind, ParseStatus, ReferenceKind, FACT_SCHEMA_VERSION};
use crate::index::{
    FileId, FileInput, ImportRecord, ReferenceRecord, Resolution, Snapshot, SnapshotBuilder,
    SymbolId, SymbolRecord, BUILD_VERSION,
};
use crate::lex::query_terms;
use crate::oid::Oid;
use crate::path::RelPath;
use crate::render::encode_anchor;
use crate::snapshot::SNAPSHOT_SCHEMA_VERSION;
use crate::{Error, Result};

/// Version of the `impact` JSON contract.
///
/// Its own number rather than a shared one: the rule is
/// [`crate::json_contract`], and a counter added to `rr refresh` must not make
/// an `impact` consumer re-validate an object that did not change.
pub const IMPACT_SCHEMA_VERSION: u32 = 1;

/// The command name this surface publishes.
pub const IMPACT_COMMAND: &str = "impact";

/// Traversal depth when the caller names none.
pub const DEFAULT_DEPTH: u8 = 2;

/// The deepest closure this command will compute.
///
/// A bound rather than a preference: past a handful of hops every definition in
/// a repository is reachable from every other one, and a report that names them
/// all has stopped being evidence about a change.
pub const MAX_DEPTH: u8 = 8;

/// Entries rendered per category before `shown/total` truncation.
pub const DEFAULT_LIMIT: u32 = 50;

/// The largest rendering limit this command accepts.
pub const MAX_LIMIT: u32 = 1_000;

/// A path whose bytes moved between the observation and the read.
pub const IMPACT_WORKTREE_RACED: &str = "IMPACT_WORKTREE_RACED";

/// A path with unmerged index stages, which has no single base side.
pub const IMPACT_CONFLICTED_PATH: &str = "IMPACT_CONFLICTED_PATH";

/// Which endpoint a question is being asked about.
///
/// Distinct from [`Endpoint`], which is an *answer* a report publishes and can
/// say `both`. A question is always about one side, because the two sides hold
/// different bytes and a hunk's line numbers mean something different on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    /// The endpoint the comparison started from.
    Base,
    /// The endpoint the comparison ended at.
    Target,
}

/// Which endpoints an observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Endpoint {
    /// The base endpoint alone; for an edge, the deletion case.
    Base,
    /// The target endpoint alone; for an edge, a caller that is new.
    Target,
    /// Both endpoints agree it is there.
    Both,
}

impl Endpoint {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Target => "target",
            Self::Both => "both",
        }
    }

    /// Which endpoints hold a path, given which sides have bytes for it.
    const fn of(base: bool, target: bool) -> Self {
        match (base, target) {
            (true, true) | (false, false) => Self::Both,
            (true, false) => Self::Base,
            (false, true) => Self::Target,
        }
    }
}

/// What kind of thing one comparison endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointKind {
    /// A committed tree, pinned to the commit a revision spelling resolved to.
    Tree,
    /// The working tree: the index, unstaged edits and eligible untracked
    /// sources, observed once.
    Worktree,
}

impl EndpointKind {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Worktree => "worktree",
        }
    }
}

/// How one report names one comparison endpoint.
///
/// The caller's spelling is kept beside the commit it resolved to, because two
/// runs naming `HEAD` at different times are different reports and one that
/// recorded only the word `HEAD` could never say which of the two it was.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EndpointJson {
    /// What kind of endpoint this is.
    pub kind: EndpointKind,
    /// The revision exactly as the caller wrote it; absent for a working tree,
    /// which no spelling addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// The commit this endpoint is pinned to, in hex; absent when `HEAD` is
    /// unborn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl EndpointJson {
    /// Names a committed tree, by the spelling that resolved it and its commit.
    #[must_use]
    pub fn tree(spec: &str, commit: &Oid) -> Self {
        Self {
            kind: EndpointKind::Tree,
            spec: Some(spec.to_owned()),
            commit: Some(commit.to_hex()),
        }
    }

    /// Names the working tree, on top of the commit `HEAD` pointed at.
    #[must_use]
    pub fn worktree(head: Option<&Oid>) -> Self {
        Self {
            kind: EndpointKind::Worktree,
            spec: None,
            commit: head.map(Oid::to_hex),
        }
    }

    /// The one-line form the text report prints.
    fn as_text(&self) -> String {
        let name = self.spec.as_deref().unwrap_or(self.kind.as_str());
        match &self.commit {
            Some(commit) => format!("{name} ({commit})"),
            None => name.to_owned(),
        }
    }
}

/// Identity of a graph node that survives the base/target boundary.
///
/// [`SymbolId`] is an arena index into one snapshot, so keying by id splits one
/// definition in two across the two overlays — and a node present only on the
/// base side *is* a deletion, the case this command exists for. The pair
/// `(path, qualified name)` is the same definition on both sides whatever either
/// arena numbered it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeKey {
    /// Repository-relative path of the file that declares the node.
    pub path: Box<str>,
    /// Qualified name, or the empty string for a file-scope node.
    pub qualified: Box<str>,
}

impl NodeKey {
    /// Canonical anchor for a file-scope import that owns no definition.
    ///
    /// [`ImportRecord::owner`] is `None` for a module-scope import, and an
    /// invented owner makes every file-level `use` a call from whatever
    /// definition happens to sit above it in the file.
    #[must_use]
    pub fn file(path: &str) -> Self {
        Self {
            path: path.into(),
            qualified: Box::from(""),
        }
    }

    /// The node one definition is, named by its path and qualified name.
    #[must_use]
    pub fn definition(path: &str, qualified: &str) -> Self {
        Self {
            path: path.into(),
            qualified: qualified.into(),
        }
    }

    /// Whether this key names a file rather than a definition inside one.
    #[must_use]
    pub const fn is_file(&self) -> bool {
        self.qualified.is_empty()
    }
}

/// One changed file's line ranges, in the vocabulary impact reads them with.
///
/// The same four numbers `rr_git::diff::Hunk` publishes, declared here because
/// the dependency runs the other way round: `rr-git` depends on this crate, so a
/// graph layer in `rr-core` cannot name a Git type. The caller converts once, in
/// the same place and for the same reason `crate::check::check` takes a
/// `SnapshotLabel` as a parameter instead of asking Git for it.
///
/// Lines are 1-based and the ranges are half-open, so a pure insertion is
/// `old_lines == 0` and names the line it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HunkRange {
    /// First base-side line the hunk replaces, or the line it follows.
    pub old_start: u32,
    /// Base-side lines replaced; `0` for a pure insertion.
    pub old_lines: u32,
    /// First target-side line the hunk introduces, or the line it follows.
    pub new_start: u32,
    /// Target-side lines introduced; `0` for a pure deletion.
    pub new_lines: u32,
}

impl HunkRange {
    /// The half-open line range this hunk covers on one side.
    const fn lines(self, side: Side) -> (u32, u32) {
        match side {
            Side::Base => (self.old_start, self.old_lines),
            Side::Target => (self.new_start, self.new_lines),
        }
    }
}

/// One changed path, as impact reads a delta.
///
/// Presence is spelled by the two object ids rather than by a change kind:
/// `base_oid.is_none()` is an addition, `target_oid.is_none()` a deletion, and
/// two equal ids beside a `source` are a rename that moved bytes without
/// changing them. Deriving it here means impact cannot disagree with the delta
/// it was handed about which endpoint holds what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Target-side path; for a deletion, the path that went away.
    pub path: RelPath,
    /// Old-side path of a rename or copy.
    pub source: Option<RelPath>,
    /// Bytes the base endpoint holds; absent for an addition.
    pub base_oid: Option<Oid>,
    /// Bytes the target endpoint holds; absent for a deletion.
    pub target_oid: Option<Oid>,
    /// Zero-context hunks.
    ///
    /// Empty means *the whole file changed*, never *nothing changed*: an
    /// addition, a deletion, bytes no line diff can address, or a rename that
    /// moved the exact same bytes.
    pub hunks: Vec<HunkRange>,
}

impl FileChange {
    /// The base-side path, which a rename makes different from the target's.
    fn base_path(&self) -> &RelPath {
        self.source.as_ref().unwrap_or(&self.path)
    }

    /// Whether both endpoints hold the very same bytes.
    fn content_identical(&self) -> bool {
        self.base_oid.is_some() && self.base_oid == self.target_oid
    }
}

/// The three versions a persisted record must have been written under before an
/// endpoint may reuse it instead of reading the file again.
///
/// Taken from the caller rather than read off the snapshot value, because only
/// one of the three is recorded in it: `build_version` sits in
/// [`crate::index::SnapshotMeta`], the snapshot schema is proven by the envelope
/// the loader decoded, and the fact schema by the key the fact cache was read
/// under. A caller that cannot state all three has not proven the record
/// describes the bytes this endpoint holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaStamp {
    /// [`BUILD_VERSION`] the record was indexed under.
    pub build_version: u32,
    /// [`FACT_SCHEMA_VERSION`] its facts were extracted under.
    pub fact_schema_version: u32,
    /// [`SNAPSHOT_SCHEMA_VERSION`] its envelope was written under.
    pub snapshot_schema_version: u32,
}

impl SchemaStamp {
    /// The stamp this binary writes.
    ///
    /// Every value is read from the constant that defines it. A version copied
    /// into a second place is a version that gets bumped in one of them.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            build_version: BUILD_VERSION,
            fact_schema_version: FACT_SCHEMA_VERSION,
            snapshot_schema_version: SNAPSHOT_SCHEMA_VERSION,
        }
    }
}

/// Whether the published snapshot's records for one file may stand in for a
/// fresh read of it at one endpoint.
///
/// Four agreements, and all four are required. The file's `content_oid` must be
/// the bytes the endpoint holds, and the three schema versions must be this
/// binary's: a record written by another build of the indexer describes a
/// vocabulary this one no longer produces, and mixing the two silently would put
/// two incompatible halves of an index in one graph.
#[must_use]
pub fn carries_over(
    published: &Snapshot,
    written_under: SchemaStamp,
    path: &str,
    content_oid: Oid,
) -> bool {
    written_under == SchemaStamp::current()
        && published.meta.build_version == BUILD_VERSION
        && published
            .file_by_path(path)
            .is_some_and(|file| file.content_oid == content_oid)
}

/// Builds the in-memory snapshot one endpoint's graph is read from.
///
/// **It never touches `.rr/local/snapshot.bin`, and that is a contract rather
/// than an implementation detail.** `rr impact` answers a question; publishing
/// an overlay would turn a read-only command into a writer, and the thing it
/// would write is not even a description of the repository — it is a description
/// of one side of one comparison, which the next `rr query` would then serve as
/// if it were the working tree.
///
/// `inputs` is the whole endpoint: the files it holds, with the facts extracted
/// from *its* bytes. A path the endpoint does not hold is spelled by its
/// absence from `inputs` rather than by a removal list, so there is no second
/// way to say the same thing and no way for the two to disagree.
///
/// The build goes through [`SnapshotBuilder`], the same one `rr refresh` uses,
/// for the reason every rule in [`crate::check`] delegates: an overlay whose
/// resolver disagreed with the published index would report edges no query could
/// ever follow. It carries the published metadata verbatim — an overlay is never
/// published, so it never has to be re-identified, and carrying the metadata is
/// what keeps the builder's own validation meaningful.
///
/// # Errors
/// Returns the id, term, range and invariant failures of
/// [`SnapshotBuilder::build`].
pub fn overlay(published: &Snapshot, inputs: Vec<FileInput>) -> Result<Snapshot> {
    let (snapshot, _counts) = SnapshotBuilder::new(published.meta.clone()).build(inputs)?;
    Ok(snapshot)
}

/// Whether one report describes everything it was asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImpactStatus {
    /// Every path in the change set was evaluated, including the ones whose
    /// observations are honestly counted rather than turned into edges.
    Complete,
    /// Part of the change set could not be evaluated, and the report says which.
    Partial,
}

impl ImpactStatus {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
        }
    }
}

/// What kind of resolved observation one edge is.
///
/// Three spellings and no more, mapped from the fact vocabulary one arm each so
/// that a consumer matching on this can be exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    /// [`ReferenceKind::Call`] and [`ReferenceKind::MacroCall`].
    Call,
    /// [`ReferenceKind::Implementation`] and [`ReferenceKind::Type`].
    Reference,
    /// Every [`ImportRecord`], whatever its [`crate::facts::ImportKind`].
    Import,
}

impl EdgeKind {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Reference => "reference",
            Self::Import => "import",
        }
    }

    /// The edge one reference kind makes.
    ///
    /// [`ReferenceKind::MethodCall`] cannot arrive here: `index::build` leaves
    /// it permanently unresolved, and only a resolved record becomes an edge. It
    /// is mapped rather than refused so that the day a receiver-type resolver
    /// exists, this arm is already the honest answer instead of a panic.
    /// [`ReferenceKind::Type`] joins [`ReferenceKind::Implementation`] because
    /// neither one calls anything.
    const fn of_reference(kind: ReferenceKind) -> Self {
        match kind {
            ReferenceKind::Call | ReferenceKind::MacroCall | ReferenceKind::MethodCall => {
                Self::Call
            }
            ReferenceKind::Implementation | ReferenceKind::Type => Self::Reference,
        }
    }
}

/// What happened to one definition between the two endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefinitionChange {
    /// It is on the target side and on no base side.
    Added,
    /// Both endpoints declare it and a hunk touches it.
    Modified,
    /// It is on the base side and on no target side.
    Removed,
    /// Both endpoints declare it, unchanged, at a different anchor.
    Moved,
}

impl DefinitionChange {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Removed => "removed",
            Self::Moved => "moved",
        }
    }
}

/// How much impact could say about one changed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileState {
    /// Its definitions were mapped from its own bytes.
    Described,
    /// An endpoint could not parse it, so it has a changed file and no guessed
    /// definitions.
    Degraded,
    /// It has unmerged index stages, so it has no single base side to compare
    /// against.
    Conflicted,
}

impl FileState {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Described => "described",
            Self::Degraded => "degraded",
            Self::Conflicted => "conflicted",
        }
    }
}

/// Why one test file is worth running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestReason {
    /// A definition in the test file has a resolved edge straight to a seed.
    ResolvedReference,
    /// A definition in the test file is reached by the closure.
    AffectedTest,
    /// A definition in the test file shares a normalized name term with a
    /// changed definition. Correlational, so it is labelled probable and never
    /// reaches `affected`.
    LexicalName,
    /// The bounded historical rule reported the file as changing with the change
    /// set. Correlational, and labelled probable for the same reason.
    CoChange,
}

impl TestReason {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResolvedReference => "resolved-reference",
            Self::AffectedTest => "affected-test",
            Self::LexicalName => "lexical-name",
            Self::CoChange => "co-change",
        }
    }

    /// How much one reason is worth.
    const fn confidence(self) -> Evidence {
        match self {
            Self::ResolvedReference | Self::AffectedTest => Evidence::Exact,
            Self::LexicalName | Self::CoChange => Evidence::Probable,
        }
    }
}

/// How much one claim is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Evidence {
    /// A resolved structural edge says so.
    Exact,
    /// A correlation says so, and a correlation can be wrong.
    Probable,
}

impl Evidence {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Probable => "probable",
        }
    }
}

/// One file the change set touched.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Which endpoints hold it.
    ///
    /// A conflicted path is `both`: the base side holds two or three competing
    /// stages, which is exactly why nothing more is said about it.
    pub endpoint: Endpoint,
    /// How much impact could say about it.
    pub state: FileState,
    /// Where it moved from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Changed definitions attributed to it.
    pub definitions: u32,
}

/// One definition the change set touched.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChangedDefinition {
    /// The `rr query` anchor, from [`encode_anchor`].
    pub anchor: String,
    /// Repository-relative path of the file that declares it.
    pub path: String,
    /// Qualified name.
    pub qualified: String,
    /// Definition kind, in the fact vocabulary's own spelling.
    pub kind: DefKind,
    /// What happened to it.
    pub change: DefinitionChange,
    /// Which endpoints declare it.
    pub endpoint: Endpoint,
    /// First line of its span, on the endpoint that describes it.
    pub line: u32,
}

/// One resolved edge, merged across the two endpoints.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactEdge {
    /// What kind of observation it is.
    pub kind: EdgeKind,
    /// Anchor of the definition that points.
    pub source: String,
    /// Anchor of the definition it points at.
    pub target: String,
    /// Line of the pointing site.
    pub line: u32,
    /// Which endpoints observed it.
    pub endpoint: Endpoint,
}

/// One definition a closure reached.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactNode {
    /// The `rr query` anchor, from [`encode_anchor`].
    pub anchor: String,
    /// Repository-relative path of the file that declares it.
    pub path: String,
    /// Qualified name, or the empty string for a file-scope node.
    pub qualified: String,
    /// Length of the shortest evidence chain from a seed.
    pub distance: u8,
    /// The kind of edge that reached it at that distance.
    pub via: EdgeKind,
}

/// One strongly connected component of the induced affected subgraph.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactCycle {
    /// Shortest evidence chain to any member.
    pub distance: u8,
    /// Member anchors, sorted.
    pub members: Vec<String>,
}

/// One test file worth running, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TestImpact {
    /// Repository-relative path of the test file.
    pub path: String,
    /// The strongest thing any of its reasons can claim.
    pub confidence: Evidence,
    /// Every reason it appears, sorted and free of duplicates.
    pub reasons: Vec<TestReason>,
}

/// Observations the index could not turn into edges, grouped so a reader can
/// tell "nothing depends on this" from "we could not tell".
///
/// Every counter is about the change set: references and imports declared in the
/// changed files, and resolved edges incident to a changed definition. A
/// repository-wide total would be the same number on every run and would say
/// nothing about the change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ResolutionCounts {
    /// Resolved edges incident to a changed definition.
    pub exact_edges: u64,
    /// Call sites the index cannot pin to a definition, method calls included.
    pub unresolved_calls: u64,
    /// Type and implementation references it cannot pin to a definition.
    pub unresolved_references: u64,
    /// Imports it cannot follow; see [`UnfollowedImport`], which lists them.
    pub unresolved_imports: u64,
    /// Call sites that match more than one definition.
    pub ambiguous_calls: u64,
    /// Type and implementation references that match more than one definition.
    pub ambiguous_references: u64,
    /// Imports that match more than one definition.
    pub ambiguous_imports: u64,
    /// Changed files an endpoint could not parse.
    pub degraded_files: u64,
}

/// One import the change touches that the index cannot follow.
///
/// Listed, and not merely counted: where a language resolves no imports at all —
/// every kind but Rust's `use`, because [`crate::facts::ImportKind::resolves_by_path`] is
/// false for the rest — an empty `affected` set reads as "nothing depends on
/// this", which is the one thing it does not mean. Omitting these makes
/// `rr impact` under-report in silence; inventing a target for them makes it
/// lie.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnfollowedImport {
    /// The changed file that declares it.
    pub path: String,
    /// [`ImportRecord::path`] verbatim, as the language spells it: `./auth`,
    /// `..pkg`, `react`. Kept as written so a reader can grep for it.
    pub specifier: String,
    /// The leaf, where the language spells one apart from the specifier.
    ///
    /// A separate member and never joined onto `specifier`: `./auth` and
    /// `verifyToken` are two facts, and one path spelled out of them would be a
    /// path no resolver in any language could look up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Line of the import clause.
    pub line: u32,
    /// Always `unresolved-by-design`.
    ///
    /// A member rather than prose, so a later resolver can distinguish "cannot"
    /// from "did not".
    pub why: &'static str,
}

/// The reason every [`UnfollowedImport`] carries.
const UNRESOLVED_BY_DESIGN: &str = "unresolved-by-design";

/// One `rr impact` run, as both renderers see it.
///
/// Declaration order is JSON field order, and `schema_version` is first, per
/// [`crate::json_contract`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactResultV1 {
    /// [`IMPACT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Whether everything asked about was evaluated.
    pub status: ImpactStatus,
    /// The endpoint the comparison started from.
    pub base: EndpointJson,
    /// The endpoint the comparison ended at.
    pub target: EndpointJson,
    /// The traversal depth this run used.
    pub depth: u8,
    /// Every file the change set touched.
    pub changed_files: Vec<ChangedFile>,
    /// Every definition the change set touched: the seeds.
    pub changed_definitions: Vec<ChangedDefinition>,
    /// Resolved edges incident to a seed.
    pub direct_edges: Vec<ImpactEdge>,
    /// The reverse closure: who points at a seed.
    pub affected: Vec<ImpactNode>,
    /// The forward closure: what a seed points at.
    pub dependencies: Vec<ImpactNode>,
    /// Cycles in the induced affected subgraph.
    pub cycles: Vec<ImpactCycle>,
    /// Test files worth running, and why each one.
    pub tests: Vec<TestImpact>,
    /// What could not be turned into edges.
    pub resolution: ResolutionCounts,
    /// What went wrong, in [`crate::check`]'s diagnostic vocabulary.
    pub diagnostics: Vec<DiagnosticV1>,
    /// Imports in changed files that no resolver here can follow.
    pub unfollowed_imports: Vec<UnfollowedImport>,
}

impl ImpactResultV1 {
    /// Sorts every vector into the one order both renderers see.
    ///
    /// Called once, before either one runs: sorting inside a renderer lets text
    /// and JSON disagree about order, which is a bug that surfaces months later
    /// in a diff nobody can explain. Every key below is total over the members a
    /// reader can see, so two entries never compare equal while printing
    /// differently.
    pub fn canonicalize(&mut self) {
        self.changed_files.sort_by(|left, right| {
            (&left.path, left.state, left.endpoint).cmp(&(&right.path, right.state, right.endpoint))
        });
        self.changed_definitions.sort_by(|left, right| {
            (&left.anchor, &left.qualified, left.kind, left.change).cmp(&(
                &right.anchor,
                &right.qualified,
                right.kind,
                right.change,
            ))
        });
        self.direct_edges.sort_by(|left, right| {
            (&left.source, &left.target, left.kind, left.line).cmp(&(
                &right.source,
                &right.target,
                right.kind,
                right.line,
            ))
        });
        for nodes in [&mut self.affected, &mut self.dependencies] {
            nodes.sort_by(|left, right| {
                (left.distance, &left.anchor, left.via).cmp(&(
                    right.distance,
                    &right.anchor,
                    right.via,
                ))
            });
        }
        for cycle in &mut self.cycles {
            cycle.members.sort();
        }
        self.cycles.sort_by(|left, right| {
            (left.distance, left.members.first()).cmp(&(right.distance, right.members.first()))
        });
        for test in &mut self.tests {
            test.reasons.sort_unstable();
            test.reasons.dedup();
        }
        self.tests.sort_by(|left, right| left.path.cmp(&right.path));
        self.diagnostics.sort_by(|left, right| {
            (left.severity, left.rule_id, left.path.as_deref()).cmp(&(
                right.severity,
                right.rule_id,
                right.path.as_deref(),
            ))
        });
        self.unfollowed_imports.sort_by(|left, right| {
            (&left.path, left.line, &left.specifier, &left.name).cmp(&(
                &right.path,
                right.line,
                &right.specifier,
                &right.name,
            ))
        });
    }

    /// `0` complete, `1` partial.
    ///
    /// Failures never reach here: a run that could not be evaluated at all exits
    /// `2` from the command, which is `clap`'s code and means the same thing —
    /// the invocation could not be evaluated. `1` always means a report *was*
    /// printed and says which part of it is missing.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self.status {
            ImpactStatus::Complete => 0,
            ImpactStatus::Partial => 1,
        }
    }
}

/// Which way a closure runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards the seeds: who points at them.
    Incoming,
    /// Away from the seeds: what they point at.
    Outgoing,
}

/// One merged edge, before its ends are spelled as anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The node that points.
    pub source: NodeKey,
    /// The node it points at.
    pub target: NodeKey,
    /// What kind of observation it is.
    pub kind: EdgeKind,
    /// Line of the pointing site.
    pub line: u32,
    /// Which endpoints observed it.
    pub endpoint: Endpoint,
}

/// One node a traversal reached, before its anchor is spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    /// The node.
    pub node: NodeKey,
    /// Length of the shortest evidence chain that reaches it.
    pub distance: u8,
    /// The kind of edge that reached it at that distance.
    pub via: EdgeKind,
}

/// What the two endpoints together say about one edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeFacts {
    line: u32,
    endpoint: Endpoint,
}

/// One end of one edge, as the adjacency maps store it.
type Neighbour = (NodeKey, EdgeKind);

/// Edges from both endpoints, merged by [`NodeKey`].
///
/// Merging is required rather than an optimisation: a deleted definition has
/// callers only in the base graph, and a new caller exists only in the target,
/// so either side alone drops one of the two changes most worth reporting.
///
/// The maps are [`BTreeMap`] and [`BTreeSet`] throughout, so every walk over
/// this structure visits the same nodes in the same order on every run and on
/// every platform.
#[derive(Debug, Default)]
pub struct Graph {
    edges: BTreeMap<(NodeKey, NodeKey, EdgeKind), EdgeFacts>,
    incoming: BTreeMap<NodeKey, BTreeSet<Neighbour>>,
    outgoing: BTreeMap<NodeKey, BTreeSet<Neighbour>>,
}

impl Graph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many merged edges the graph holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether the graph holds no edges at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Records one edge, merging it with the same edge seen at the other
    /// endpoint.
    ///
    /// Identity is `(source, target, kind)` and deliberately excludes the line:
    /// one inserted line above a call site shifts it on the target side, and an
    /// edge that counted twice for that reason would report one call as two. The
    /// target's line wins, because the target is the state an agent is about to
    /// edit.
    pub fn insert(&mut self, edge: Edge) {
        self.incoming
            .entry(edge.target.clone())
            .or_default()
            .insert((edge.source.clone(), edge.kind));
        self.outgoing
            .entry(edge.source.clone())
            .or_default()
            .insert((edge.target.clone(), edge.kind));
        let facts = EdgeFacts {
            line: edge.line,
            endpoint: edge.endpoint,
        };
        self.edges
            .entry((edge.source, edge.target, edge.kind))
            .and_modify(|known| {
                if known.endpoint != edge.endpoint {
                    known.endpoint = Endpoint::Both;
                }
                if edge.endpoint == Endpoint::Target {
                    known.line = facts.line;
                }
            })
            .or_insert(facts);
    }

    /// Every edge with a seed at either end.
    #[must_use]
    pub fn incident(&self, seeds: &BTreeSet<NodeKey>) -> Vec<Edge> {
        self.edges
            .iter()
            .filter(|((source, target, _), _)| seeds.contains(source) || seeds.contains(target))
            .map(|((source, target, kind), facts)| Edge {
                source: source.clone(),
                target: target.clone(),
                kind: *kind,
                line: facts.line,
                endpoint: facts.endpoint,
            })
            .collect()
    }

    /// Breadth-first closure from `seeds`, bounded by `depth`.
    ///
    /// The minimum-distance map is what makes `distance` mean "shortest evidence
    /// chain" rather than "whichever path came first" — which would tie every
    /// number in the report to [`BTreeMap`] iteration order — and it is also
    /// what terminates the walk on a cycle.
    ///
    /// A seed keeps distance `0` even when a chain returns to it: a definition
    /// cannot be further from the change than the change itself. Seeds are
    /// listed only when an edge actually reaches them, because a seed that
    /// nothing points at belongs in `changed_definitions` and nowhere else.
    #[must_use]
    pub fn traverse(
        &self,
        seeds: &BTreeSet<NodeKey>,
        direction: Direction,
        depth: u8,
    ) -> Vec<Reached> {
        let mut distance: BTreeMap<&NodeKey, u8> = seeds.iter().map(|seed| (seed, 0)).collect();
        let mut reached: BTreeMap<&NodeKey, (u8, EdgeKind)> = BTreeMap::new();
        let mut queue: VecDeque<(&NodeKey, u8)> = seeds.iter().map(|seed| (seed, 0)).collect();

        while let Some((node, walked)) = queue.pop_front() {
            if walked >= depth {
                continue;
            }
            let step = walked.saturating_add(1);
            for (neighbour, kind) in self.neighbours(node, direction) {
                match reached.get_mut(neighbour) {
                    Some(known) if known.0 < step => {}
                    Some(known) if known.0 == step => known.1 = known.1.min(*kind),
                    Some(known) => *known = (step, *kind),
                    None => {
                        reached.insert(neighbour, (step, *kind));
                    }
                }
                let known = distance.get(neighbour).copied();
                if known.is_none_or(|previous| step < previous) {
                    distance.insert(neighbour, step);
                    queue.push_back((neighbour, step));
                }
            }
        }

        reached
            .into_iter()
            .map(|(node, (step, via))| Reached {
                node: node.clone(),
                distance: distance.get(node).copied().unwrap_or(step),
                via,
            })
            .collect()
    }

    /// Cycles in the subgraph induced by `nodes`.
    ///
    /// Tarjan's algorithm, **iteratively**: the recursive form recurses once per
    /// edge along a chain, and the 10,000-file corpus overflows the stack long
    /// before it finishes. The explicit work stack makes the depth a heap
    /// allocation instead.
    ///
    /// Components of size one are dropped unless the node points at itself,
    /// because "this definition is in a cycle with nothing" is not a finding.
    #[must_use]
    pub fn cycles(&self, nodes: &BTreeSet<NodeKey>) -> Vec<Vec<NodeKey>> {
        let members: Vec<&NodeKey> = nodes.iter().collect();
        let position: BTreeMap<&NodeKey, usize> = members
            .iter()
            .enumerate()
            .map(|(index, node)| (*node, index))
            .collect();
        let adjacency: Vec<Vec<usize>> = members
            .iter()
            .map(|node| {
                self.neighbours(node, Direction::Outgoing)
                    .filter_map(|(neighbour, _)| position.get(neighbour).copied())
                    .collect()
            })
            .collect();

        let mut components = tarjan(&adjacency);
        components.retain(|component| {
            component.len() > 1
                || component
                    .first()
                    .is_some_and(|node| adjacency[*node].contains(node))
        });
        components
            .into_iter()
            .map(|component| {
                component
                    .into_iter()
                    .filter_map(|index| members.get(index).map(|node| (*node).clone()))
                    .collect()
            })
            .collect()
    }

    /// One node's neighbours in one direction.
    fn neighbours(
        &self,
        node: &NodeKey,
        direction: Direction,
    ) -> impl Iterator<Item = (&NodeKey, &EdgeKind)> {
        let side = match direction {
            Direction::Incoming => &self.incoming,
            Direction::Outgoing => &self.outgoing,
        };
        side.get(node)
            .into_iter()
            .flatten()
            .map(|(neighbour, kind)| (neighbour, kind))
    }
}

/// Every strongly connected component of an adjacency list, iteratively.
///
/// Tarjan's algorithm with the recursion replaced by an explicit stack of
/// `(node, next child)` frames. Components come out in the order the algorithm
/// finishes them, which the caller sorts; nothing here depends on that order.
fn tarjan(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;

    let count = adjacency.len();
    let mut index = vec![UNVISITED; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut work: Vec<(usize, usize)> = Vec::new();
    let mut next = 0usize;
    let mut components: Vec<Vec<usize>> = Vec::new();

    for root in 0..count {
        if index[root] != UNVISITED {
            continue;
        }
        work.push((root, 0));
        while let Some((node, child)) = work.pop() {
            if child == 0 {
                index[node] = next;
                low[node] = next;
                next += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            let mut descended = false;
            for (offset, neighbour) in adjacency[node].iter().enumerate().skip(child) {
                if index[*neighbour] == UNVISITED {
                    work.push((node, offset + 1));
                    work.push((*neighbour, 0));
                    descended = true;
                    break;
                }
                if on_stack[*neighbour] {
                    low[node] = low[node].min(index[*neighbour]);
                }
            }
            if descended {
                continue;
            }
            if low[node] == index[node] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
            if let Some((parent, _)) = work.last() {
                low[*parent] = low[*parent].min(low[node]);
            }
        }
    }
    components
}

/// Everything one impact run reads.
///
/// The two overlays are the graph; the change set says which definitions the
/// deltas touch. Nothing here reads a file, opens a repository or asks Git a
/// question: the endpoints were resolved once, by the caller, before any of this
/// was assembled, which is what makes two runs of the same comparison the same
/// report.
#[derive(Debug)]
pub struct ImpactRequest<'run> {
    /// The base endpoint, as the report names it.
    pub base: EndpointJson,
    /// The target endpoint, as the report names it.
    pub target: EndpointJson,
    /// The base endpoint's overlay.
    pub base_snapshot: &'run Snapshot,
    /// The target endpoint's overlay.
    pub target_snapshot: &'run Snapshot,
    /// One entry per changed path.
    pub changes: &'run [FileChange],
    /// Paths whose bytes moved between the observation and the read.
    pub raced: &'run [RelPath],
    /// Paths with unmerged index stages.
    pub conflicted: &'run [RelPath],
    /// Files the bounded historical rule reported as changing with the change
    /// set.
    ///
    /// A parameter and not a computation: reading history is `rr-git`'s, and
    /// `rr-git` depends on this crate rather than the other way round. Only the
    /// paths are taken, never the thresholds — a rule this module could restate
    /// is a rule two modules could disagree about.
    pub co_changed: &'run [RelPath],
    /// Traversal depth; `0` reports incident edges and no closure.
    pub depth: u8,
}

/// Which definitions one change reaches, and the evidence for every claim.
///
/// # Errors
/// Returns [`Error::SnapshotInvariant`] when an overlay names a string, symbol
/// or file that is not in it — which validation proves cannot happen, and which
/// is therefore reported rather than papered over with an empty path.
pub fn impact(request: &ImpactRequest<'_>) -> Result<ImpactResultV1> {
    let depth = request.depth.min(MAX_DEPTH);
    let raced: BTreeSet<&str> = request.raced.iter().map(RelPath::as_str).collect();
    let conflicted: BTreeSet<&str> = request.conflicted.iter().map(RelPath::as_str).collect();

    let mut names = BTreeMap::new();
    let mut graph = Graph::new();
    absorb(request.base_snapshot, Side::Base, &mut graph, &mut names)?;
    absorb(
        request.target_snapshot,
        Side::Target,
        &mut graph,
        &mut names,
    )?;

    let mut mapped = map_changes(request, &raced, &conflicted)?;
    let seeds: BTreeSet<NodeKey> = mapped
        .definitions
        .iter()
        .map(|definition| NodeKey::definition(&definition.path, &definition.qualified))
        .collect();

    let direct = graph.incident(&seeds);
    mapped.counts.exact_edges = count(direct.len());
    let affected = graph.traverse(&seeds, Direction::Incoming, depth);
    let dependencies = graph.traverse(&seeds, Direction::Outgoing, depth);

    let mut induced: BTreeSet<NodeKey> = seeds.clone();
    induced.extend(affected.iter().map(|node| node.node.clone()));
    let distances: BTreeMap<&NodeKey, u8> = affected
        .iter()
        .map(|node| (&node.node, node.distance))
        .collect();
    let cycles = graph
        .cycles(&induced)
        .into_iter()
        .map(|component| ImpactCycle {
            distance: component
                .iter()
                .map(|node| distances.get(node).copied().unwrap_or(0))
                .min()
                .unwrap_or(0),
            members: component
                .iter()
                .map(|node| anchor_of(node, &names))
                .collect(),
        })
        .collect();

    let tests = test_evidence(request, &mapped, &affected, &dependencies)?;
    let status = if request.raced.is_empty() {
        ImpactStatus::Complete
    } else {
        ImpactStatus::Partial
    };

    let mut result = ImpactResultV1 {
        schema_version: IMPACT_SCHEMA_VERSION,
        status,
        base: request.base.clone(),
        target: request.target.clone(),
        depth,
        changed_files: mapped.files,
        changed_definitions: mapped.definitions,
        direct_edges: direct
            .into_iter()
            .map(|edge| ImpactEdge {
                kind: edge.kind,
                source: anchor_of(&edge.source, &names),
                target: anchor_of(&edge.target, &names),
                line: edge.line,
                endpoint: edge.endpoint,
            })
            .collect(),
        affected: nodes_of(affected, &names),
        dependencies: nodes_of(dependencies, &names),
        cycles,
        tests,
        resolution: mapped.counts,
        diagnostics: mapped.diagnostics,
        unfollowed_imports: mapped.unfollowed,
    };
    result.canonicalize();
    Ok(result)
}

/// The `rr query` anchor one node is spelled as.
///
/// Through [`encode_anchor`], which owns the spelling, and with the bare name
/// rather than the qualified one because that is the anchor
/// [`crate::query::resolve_route_anchor`] resolves. The qualified name stays in
/// [`ImpactNode::qualified`], where it is the identity rather than the address.
fn anchor_of(node: &NodeKey, names: &BTreeMap<NodeKey, Box<str>>) -> String {
    encode_anchor(&node.path, names.get(node).map(AsRef::as_ref))
}

/// Spells every reached node as a reported one.
fn nodes_of(reached: Vec<Reached>, names: &BTreeMap<NodeKey, Box<str>>) -> Vec<ImpactNode> {
    reached
        .into_iter()
        .map(|node| ImpactNode {
            anchor: anchor_of(&node.node, names),
            path: node.node.path.to_string(),
            qualified: node.node.qualified.to_string(),
            distance: node.distance,
            via: node.via,
        })
        .collect()
}

/// One endpoint's resolved edges and node names, added to the merged graph.
///
/// Only [`Resolution::Resolved`] produces an edge. Unresolved and ambiguous
/// records are counted by [`map_changes`], over the changed files alone.
fn absorb(
    snapshot: &Snapshot,
    side: Side,
    graph: &mut Graph,
    names: &mut BTreeMap<NodeKey, Box<str>>,
) -> Result<()> {
    let endpoint = match side {
        Side::Base => Endpoint::Base,
        Side::Target => Endpoint::Target,
    };
    for symbol in &snapshot.symbols {
        let node = node_of_symbol(snapshot, symbol)?;
        names.insert(node, Box::from(string_of(snapshot, symbol.name)?));
    }
    for reference in &snapshot.references {
        let Resolution::Resolved(resolved) = reference.resolution else {
            continue;
        };
        graph.insert(Edge {
            source: owner_node(snapshot, reference.file, reference.owner)?,
            target: node_of_id(snapshot, resolved)?,
            kind: EdgeKind::of_reference(reference.kind),
            line: reference.span.start_line(),
            endpoint,
        });
    }
    for import in &snapshot.imports {
        let Resolution::Resolved(resolved) = import.resolution else {
            continue;
        };
        graph.insert(Edge {
            source: owner_node(snapshot, import.file, import.owner)?,
            target: node_of_id(snapshot, resolved)?,
            kind: EdgeKind::Import,
            line: import.span.start_line(),
            endpoint,
        });
    }
    Ok(())
}

/// What the change set maps to, before it is spelled as a report.
struct Mapped {
    files: Vec<ChangedFile>,
    definitions: Vec<ChangedDefinition>,
    counts: ResolutionCounts,
    diagnostics: Vec<DiagnosticV1>,
    unfollowed: Vec<UnfollowedImport>,
    /// Normalized name terms of the changed definitions, for the lexical
    /// reason, keyed by the target overlay's term ids.
    changed_terms: BTreeSet<crate::lex::TermId>,
}

/// Every changed file and definition, with the counters the change set implies.
fn map_changes(
    request: &ImpactRequest<'_>,
    raced: &BTreeSet<&str>,
    conflicted: &BTreeSet<&str>,
) -> Result<Mapped> {
    let mut mapped = Mapped {
        files: Vec::new(),
        definitions: Vec::new(),
        counts: ResolutionCounts::default(),
        diagnostics: Vec::new(),
        unfollowed: Vec::new(),
        changed_terms: BTreeSet::new(),
    };

    for path in raced {
        mapped.diagnostics.push(raced_path(path));
    }
    for path in conflicted {
        mapped.diagnostics.push(conflicted_path(path));
        mapped.files.push(ChangedFile {
            path: (*path).to_owned(),
            endpoint: Endpoint::Both,
            state: FileState::Conflicted,
            source: None,
            definitions: 0,
        });
    }

    for change in request.changes {
        let path = change.path.as_str();
        if raced.contains(&path) || conflicted.contains(&path) {
            continue;
        }
        let definitions = changed_definitions(request, change)?;
        let degraded = is_degraded(request.base_snapshot, change.base_path().as_str())
            || is_degraded(request.target_snapshot, path);
        if degraded {
            mapped.counts.degraded_files += 1;
        }
        mapped.files.push(ChangedFile {
            path: path.to_owned(),
            endpoint: Endpoint::of(change.base_oid.is_some(), change.target_oid.is_some()),
            state: if degraded {
                FileState::Degraded
            } else {
                FileState::Described
            },
            source: change
                .source
                .as_ref()
                .map(|source| source.as_str().to_owned()),
            definitions: count_u32(definitions.len()),
        });
        for definition in &definitions {
            let terms = query_terms(&definition.name, request.target_snapshot);
            mapped.changed_terms.extend(terms.iter());
        }
        mapped.definitions.extend(
            definitions
                .into_iter()
                .map(|definition| definition.reported),
        );
        count_observations(request, change, &mut mapped)?;
    }
    Ok(mapped)
}

/// One changed definition, and the bare name the lexical reason reads.
struct Seed {
    reported: ChangedDefinition,
    name: String,
}

/// Every definition one changed file's two versions disagree about.
///
/// Presence decides `added` and `removed`, and a hunk decides `modified`: a
/// definition the target does not declare is gone whether or not a line diff
/// covers it, and a definition both sides declare has only changed if something
/// in it did. `moved` is the remaining case — both sides declare it, nothing
/// touched it, and its anchor is different, which is what a file rename does to
/// every definition in the file at once.
fn changed_definitions(request: &ImpactRequest<'_>, change: &FileChange) -> Result<Vec<Seed>> {
    let base = file_symbols(request.base_snapshot, change.base_path().as_str());
    let target = file_symbols(request.target_snapshot, change.path.as_str());

    let whole_file = change.hunks.is_empty() && !change.content_identical();
    let base_touched = touched_set(base, &change.hunks, Side::Base, whole_file);
    let target_touched = touched_set(target, &change.hunks, Side::Target, whole_file);
    let pairing = pair(
        request.base_snapshot,
        base,
        request.target_snapshot,
        target,
        change.source.is_some(),
    )?;

    let mut seeds = Vec::new();
    for (base_index, target_index) in pairing.matched {
        let (Some(before), Some(after)) = (base.get(base_index), target.get(target_index)) else {
            continue;
        };
        let change_kind = if base_touched.contains(&before.id) || target_touched.contains(&after.id)
        {
            DefinitionChange::Modified
        } else if anchor_of_symbol(request.base_snapshot, before)?
            != anchor_of_symbol(request.target_snapshot, after)?
        {
            DefinitionChange::Moved
        } else {
            continue;
        };
        seeds.push(seed(
            request.target_snapshot,
            after,
            change_kind,
            Endpoint::Both,
        )?);
    }
    for index in pairing.base_only {
        if let Some(symbol) = base.get(index) {
            seeds.push(seed(
                request.base_snapshot,
                symbol,
                DefinitionChange::Removed,
                Endpoint::Base,
            )?);
        }
    }
    for index in pairing.target_only {
        if let Some(symbol) = target.get(index) {
            seeds.push(seed(
                request.target_snapshot,
                symbol,
                DefinitionChange::Added,
                Endpoint::Target,
            )?);
        }
    }
    Ok(seeds)
}

/// One reported definition, described by the endpoint that still holds it.
fn seed(
    snapshot: &Snapshot,
    symbol: &SymbolRecord,
    change: DefinitionChange,
    endpoint: Endpoint,
) -> Result<Seed> {
    let path = path_of(snapshot, symbol.file)?;
    let name = string_of(snapshot, symbol.name)?;
    Ok(Seed {
        reported: ChangedDefinition {
            anchor: encode_anchor(path, Some(name)),
            path: path.to_owned(),
            qualified: string_of(snapshot, symbol.qualified_name)?.to_owned(),
            kind: symbol.kind,
            change,
            endpoint,
            line: symbol.span.start_line(),
        },
        name: name.to_owned(),
    })
}

/// One file's definitions, paired across the two endpoints.
struct Pairing {
    /// Indices into the base and target slices of one definition.
    matched: Vec<(usize, usize)>,
    /// Base-side definitions with no counterpart: removals.
    base_only: Vec<usize>,
    /// Target-side definitions with no counterpart: additions.
    target_only: Vec<usize>,
}

/// Pairs one file's two versions by `(kind, qualified name)`, then — inside a
/// rename, and only when nothing paired by name — by stable span order.
///
/// Never by fuzzy name similarity. A pairing that guessed `verify_token` and
/// `verify_tokens` were the same definition would report one `modified` where the
/// truth is one removed and one added, and every caller of the old name would be
/// missing from a report that looked complete.
///
/// The positional fallback exists for one case: a file rename moves the module
/// prefix, which changes *every* qualified name in the file at once, and pairing
/// by name would then call a moved file a wholesale rewrite. It is used only when
/// no definition paired by name, because a file where some names paired is a file
/// whose names are comparable — and there the honest answer for an unpaired
/// definition is one removal and one addition.
fn pair(
    base_snapshot: &Snapshot,
    base: &[SymbolRecord],
    target_snapshot: &Snapshot,
    target: &[SymbolRecord],
    renamed: bool,
) -> Result<Pairing> {
    let mut by_name: BTreeMap<(DefKind, String), VecDeque<usize>> = BTreeMap::new();
    for (index, symbol) in base.iter().enumerate() {
        by_name
            .entry((
                symbol.kind,
                string_of(base_snapshot, symbol.qualified_name)?.to_owned(),
            ))
            .or_default()
            .push_back(index);
    }

    let mut matched = Vec::new();
    let mut target_only = Vec::new();
    let mut paired = vec![false; base.len()];
    for (index, symbol) in target.iter().enumerate() {
        let key = (
            symbol.kind,
            string_of(target_snapshot, symbol.qualified_name)?.to_owned(),
        );
        match by_name.get_mut(&key).and_then(VecDeque::pop_front) {
            Some(before) => {
                if let Some(slot) = paired.get_mut(before) {
                    *slot = true;
                }
                matched.push((before, index));
            }
            None => target_only.push(index),
        }
    }
    let mut base_only: Vec<usize> = (0..base.len())
        .filter(|index| paired.get(*index) != Some(&true))
        .collect();

    if renamed && matched.is_empty() {
        let mut before = std::mem::take(&mut base_only).into_iter();
        let mut after = std::mem::take(&mut target_only).into_iter();
        loop {
            match (before.next(), after.next()) {
                (Some(left), Some(right)) => {
                    if base.get(left).map(|symbol| symbol.kind)
                        == target.get(right).map(|symbol| symbol.kind)
                    {
                        matched.push((left, right));
                    } else {
                        base_only.push(left);
                        target_only.push(right);
                    }
                }
                (Some(left), None) => base_only.push(left),
                (None, Some(right)) => target_only.push(right),
                (None, None) => break,
            }
        }
    }

    matched.sort_unstable();
    base_only.sort_unstable();
    target_only.sort_unstable();
    Ok(Pairing {
        matched,
        base_only,
        target_only,
    })
}

/// The `rr query` anchor one definition is spelled as, in its own snapshot.
fn anchor_of_symbol(snapshot: &Snapshot, symbol: &SymbolRecord) -> Result<String> {
    Ok(encode_anchor(
        path_of(snapshot, symbol.file)?,
        Some(string_of(snapshot, symbol.name)?),
    ))
}

/// Which of one file's definitions the hunks touch on one side.
fn touched_set(
    symbols: &[SymbolRecord],
    hunks: &[HunkRange],
    side: Side,
    whole_file: bool,
) -> BTreeSet<SymbolId> {
    if whole_file {
        return symbols.iter().map(|symbol| symbol.id).collect();
    }
    let mut out = BTreeSet::new();
    for hunk in hunks {
        out.extend(touched(symbols, *hunk, side));
    }
    out
}

/// Which definitions one hunk touches, on the side the hunk is about.
///
/// The innermost containing definition, in both of the shapes a hunk comes in.
///
/// A zero-width insertion (`old_lines == 0` on the base side, `new_lines == 0`
/// on the target side) names a point rather than a range, and goes to the
/// smallest definition containing it. A range goes to every definition it
/// intersects, *minus* the ones that fully contain another definition it also
/// intersects — so an edit inside one method marks that method and not the
/// `impl` around it, while an edit that spans two methods marks both.
///
/// The outermost rule is what this avoids: an `impl` marked changed because one
/// method inside it gained a line puts every caller of every *other* method in
/// that block into `affected`, and an agent then edits code the change never
/// reached.
///
/// Ties are broken by span length, then start byte, then id — never by name
/// similarity. Two definitions of the same size at the same point are ordered by
/// where they start, which is a fact about the file rather than a guess about the
/// author's intent.
fn touched(symbols: &[SymbolRecord], hunk: HunkRange, side: Side) -> Vec<SymbolId> {
    let (start, lines) = hunk.lines(side);
    if lines == 0 {
        let point = start.max(1);
        let innermost = symbols
            .iter()
            .filter(|symbol| symbol.span.start_line() <= point && point <= symbol.span.end_line())
            .min_by_key(|symbol| (symbol.span.byte_len(), symbol.span.start_byte(), symbol.id));
        return innermost.map(|symbol| symbol.id).into_iter().collect();
    }
    let end = start.saturating_add(lines);
    let hit: Vec<&SymbolRecord> = symbols
        .iter()
        .filter(|symbol| symbol.span.start_line() < end && start <= symbol.span.end_line())
        .collect();
    hit.iter()
        .filter(|outer| !hit.iter().any(|inner| encloses(outer, inner)))
        .map(|symbol| symbol.id)
        .collect()
}

/// Whether `outer` strictly contains `inner`.
///
/// Strictly by length, so two definitions that occupy the very same span each
/// survive: a rule under which each enclosed the other would drop both and
/// report a changed file with no changed definitions in it.
fn encloses(outer: &SymbolRecord, inner: &SymbolRecord) -> bool {
    let outer_end = outer
        .span
        .start_byte()
        .saturating_add(outer.span.byte_len());
    let inner_end = inner
        .span
        .start_byte()
        .saturating_add(inner.span.byte_len());
    outer.id != inner.id
        && outer.span.byte_len() > inner.span.byte_len()
        && outer.span.start_byte() <= inner.span.start_byte()
        && inner_end <= outer_end
}

/// The counters and unfollowed imports one changed file contributes.
///
/// The target endpoint's records, and the base endpoint's only for a path the
/// target no longer holds. One observation is counted once: the same call site
/// exists in both overlays, and counting both would report every unchanged line
/// of a changed file twice.
fn count_observations(
    request: &ImpactRequest<'_>,
    change: &FileChange,
    mapped: &mut Mapped,
) -> Result<()> {
    let (snapshot, path) = if change.target_oid.is_some() {
        (request.target_snapshot, change.path.as_str())
    } else {
        (request.base_snapshot, change.base_path().as_str())
    };
    for reference in file_references(snapshot, path) {
        let slot = match (reference.resolution, EdgeKind::of_reference(reference.kind)) {
            (Resolution::Resolved(_), _) => continue,
            (Resolution::Unresolved, EdgeKind::Call) => &mut mapped.counts.unresolved_calls,
            (Resolution::Unresolved, _) => &mut mapped.counts.unresolved_references,
            (Resolution::Ambiguous { .. }, EdgeKind::Call) => &mut mapped.counts.ambiguous_calls,
            (Resolution::Ambiguous { .. }, _) => &mut mapped.counts.ambiguous_references,
        };
        *slot += 1;
    }
    for import in file_imports(snapshot, path) {
        match import.resolution {
            Resolution::Resolved(_) => {}
            Resolution::Ambiguous { .. } => mapped.counts.ambiguous_imports += 1,
            Resolution::Unresolved => {
                mapped.counts.unresolved_imports += 1;
                mapped.unfollowed.push(UnfollowedImport {
                    path: path.to_owned(),
                    specifier: string_of(snapshot, import.path)?.to_owned(),
                    name: import
                        .name
                        .map(|name| string_of(snapshot, name).map(str::to_owned))
                        .transpose()?,
                    line: import.span.start_line(),
                    why: UNRESOLVED_BY_DESIGN,
                });
            }
        }
    }
    Ok(())
}

/// Every test file worth running, and why each one.
///
/// Four reasons, and only two of them are exact. `resolved-reference` and
/// `affected-test` are resolved edges; `lexical-name` and `co-change` are
/// correlations, which is why they are labelled probable and why neither ever
/// puts a definition in `affected`. A test named for the wrong reason costs a
/// developer one test run; a definition in `affected` for the wrong reason costs
/// an agent an edit to code that never needed it.
fn test_evidence(
    request: &ImpactRequest<'_>,
    mapped: &Mapped,
    affected: &[Reached],
    dependencies: &[Reached],
) -> Result<Vec<TestImpact>> {
    let snapshot = request.target_snapshot;
    let mut reasons: BTreeMap<String, BTreeSet<TestReason>> = BTreeMap::new();

    for node in affected.iter().chain(dependencies) {
        if !is_test(snapshot, &node.node.path) {
            continue;
        }
        let reason = if node.distance <= 1 {
            TestReason::ResolvedReference
        } else {
            TestReason::AffectedTest
        };
        reasons
            .entry(node.node.path.to_string())
            .or_default()
            .insert(reason);
    }

    for (symbol, path) in lexical_matches(snapshot, &mapped.changed_terms)? {
        let _ = symbol;
        reasons
            .entry(path)
            .or_default()
            .insert(TestReason::LexicalName);
    }

    for path in request.co_changed {
        if is_test(snapshot, path.as_str()) {
            reasons
                .entry(path.as_str().to_owned())
                .or_default()
                .insert(TestReason::CoChange);
        }
    }

    Ok(reasons
        .into_iter()
        .map(|(path, reasons)| TestImpact {
            path,
            confidence: reasons
                .iter()
                .map(|reason| reason.confidence())
                .min()
                .unwrap_or(Evidence::Probable),
            reasons: reasons.into_iter().collect(),
        })
        .collect())
}

/// Test-file definitions sharing a normalized name term with a changed one.
///
/// The `name` field alone. The body and documentation fields would match half a
/// repository on a common word, and a test list that names every file is a test
/// list nobody runs.
fn lexical_matches(
    snapshot: &Snapshot,
    terms: &BTreeSet<crate::lex::TermId>,
) -> Result<Vec<(SymbolId, String)>> {
    let mut out = Vec::new();
    for list in snapshot.postings.lists(crate::lex::LexicalField::Name) {
        if !terms.contains(&list.term) {
            continue;
        }
        for posting in &list.entries {
            let symbol =
                snapshot
                    .symbols
                    .get(posting.symbol.index())
                    .ok_or(Error::SnapshotInvariant {
                        reason: "posting symbol out of range",
                    })?;
            let path = path_of(snapshot, symbol.file)?;
            if is_test(snapshot, path) {
                out.push((symbol.id, path.to_owned()));
            }
        }
    }
    Ok(out)
}

/// Whether one path is a test file, by the classification the index stored.
///
/// Read from [`crate::index::FileRecord::is_test`] rather than re-derived, so
/// impact, the text projection and the checker cannot disagree about what a test
/// file is.
fn is_test(snapshot: &Snapshot, path: &str) -> bool {
    snapshot.file_by_path(path).is_some_and(|file| file.is_test)
}

/// Whether one endpoint failed to parse a file it holds.
fn is_degraded(snapshot: &Snapshot, path: &str) -> bool {
    snapshot
        .file_by_path(path)
        .is_some_and(|file| matches!(file.parse_status, ParseStatus::Degraded { .. }))
}

/// One file's definitions, in the snapshot's own order.
fn file_symbols<'snapshot>(snapshot: &'snapshot Snapshot, path: &str) -> &'snapshot [SymbolRecord] {
    let Some(file) = snapshot.file_by_path(path) else {
        return &[];
    };
    let start = file.first_symbol as usize;
    let end = start + file.symbol_count as usize;
    snapshot.symbols.get(start..end).unwrap_or(&[])
}

/// One file's references, in the snapshot's own order.
fn file_references<'snapshot>(
    snapshot: &'snapshot Snapshot,
    path: &str,
) -> &'snapshot [ReferenceRecord] {
    let Some(file) = snapshot.file_by_path(path) else {
        return &[];
    };
    let start = file.first_reference as usize;
    let end = start + file.reference_count as usize;
    snapshot.references.get(start..end).unwrap_or(&[])
}

/// One file's imports, in the snapshot's own order.
fn file_imports<'snapshot>(snapshot: &'snapshot Snapshot, path: &str) -> &'snapshot [ImportRecord] {
    let Some(file) = snapshot.file_by_path(path) else {
        return &[];
    };
    let start = file.first_import as usize;
    let end = start + file.import_count as usize;
    snapshot.imports.get(start..end).unwrap_or(&[])
}

/// The node one definition is, in the snapshot that holds it.
fn node_of_symbol(snapshot: &Snapshot, symbol: &SymbolRecord) -> Result<NodeKey> {
    Ok(NodeKey::definition(
        path_of(snapshot, symbol.file)?,
        string_of(snapshot, symbol.qualified_name)?,
    ))
}

/// The node one symbol id is.
fn node_of_id(snapshot: &Snapshot, symbol: SymbolId) -> Result<NodeKey> {
    let record = snapshot
        .symbols
        .get(symbol.index())
        .ok_or(Error::SnapshotInvariant {
            reason: "resolved symbol out of range",
        })?;
    node_of_symbol(snapshot, record)
}

/// The node one record's owner is, or the file itself when it has none.
fn owner_node(snapshot: &Snapshot, file: FileId, owner: Option<SymbolId>) -> Result<NodeKey> {
    match owner {
        Some(symbol) => node_of_id(snapshot, symbol),
        None => Ok(NodeKey::file(path_of(snapshot, file)?)),
    }
}

/// One file's recorded path.
fn path_of(snapshot: &Snapshot, file: FileId) -> Result<&str> {
    snapshot
        .files
        .get(file.index())
        .and_then(|record| snapshot.string(record.path))
        .ok_or(Error::SnapshotInvariant {
            reason: "file path string out of range",
        })
}

/// One interned string.
fn string_of(snapshot: &Snapshot, id: crate::index::StringId) -> Result<&str> {
    snapshot.string(id).ok_or(Error::SnapshotInvariant {
        reason: "record string out of range",
    })
}

/// [`IMPACT_WORKTREE_RACED`]: the bytes moved, so no delta describes them.
///
/// An error and not a warning: the report is missing a file it was asked about,
/// and the run is partial. A caller that treated it as advice would publish a
/// closure with a hole in it.
fn raced_path(path: &str) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: IMPACT_WORKTREE_RACED,
        severity: Severity::Error,
        message: String::from("content changed between the observation and the read"),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: None,
        remediation: "re-run `rr impact` once the file has settled",
    }
}

/// [`IMPACT_CONFLICTED_PATH`]: two or three base sides, so no comparison.
///
/// A warning: the repository is in a state its owner created deliberately, and
/// the honest answer is to list the path and say nothing else about it. Picking
/// a stage would be rr inventing the merge resolution nobody has made yet.
fn conflicted_path(path: &str) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: IMPACT_CONFLICTED_PATH,
        severity: Severity::Warning,
        message: String::from("unmerged index stages; the path has no single base side"),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: None,
        remediation: "resolve the conflict, then re-run `rr impact`",
    }
}

/// A count as the report publishes it.
fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// A count as one file's record publishes it.
fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

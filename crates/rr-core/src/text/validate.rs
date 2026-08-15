//! What is on disk, compared against what the snapshot says should be.
//!
//! This module answers one question per artifact — fresh, stale, missing, or
//! conflicting — and never acts on the answer. Acting is the caller's job,
//! because the caller holds the publication guard and this does not.
//!
//! Conflicts are collected, not thrown. A run that stops at the first problem
//! makes a human fix one file, run again, and find the next; the contract here
//! is that one run names every file that needs attention.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use crate::index::{Snapshot, SymbolId};
use crate::path::RelPath;

use super::digest::{ApiHash, Digest};
use super::model::TextProjection;
use super::purpose::read_existing_purposes;
use super::{parse_map, parse_symbols, ArtifactKind, RenderedArtifactSet};

/// How one artifact on disk compares to the generation that should replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    /// Byte-identical to what would be written. Must not be replaced.
    Fresh,
    /// Owned by rr, but describing a different projection.
    Stale,
    /// Not present.
    Missing,
    /// Present and not safe to touch.
    Conflicting,
}

/// Why one path cannot be written.
///
/// A closed set rather than a string, because the CLI prints these and issue
/// #14 will branch on them. A message is a description; this is a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictReason {
    /// A file exists at a reserved path that rr did not write.
    NotOwned,
    /// The file holds Git conflict markers.
    MergeConflict,
    /// The frontmatter is not the supported subset.
    Frontmatter,
    /// The declared `format` is not one this binary writes.
    UnsupportedFormat,
    /// A marker line is missing, duplicated, or altered.
    Marker,
    /// The purpose slot is oversize, malformed, or holds a marker.
    Purpose,
    /// A generated region was edited: `generated_hash` no longer matches.
    GeneratedEdited,
    /// A link destination or source anchor does not decode.
    Anchor,
    /// The managed `.rr/.gitignore` markers are duplicated or malformed.
    ManagedIgnore,
    /// The file could not be read.
    Unreadable,
}

impl ConflictReason {
    /// The one-line explanation the CLI prints after the path.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotOwned => "path is not owned by rr",
            Self::MergeConflict => "file contains Git conflict markers",
            Self::Frontmatter => "frontmatter is not the supported format",
            Self::UnsupportedFormat => "file declares an unsupported format version",
            Self::Marker => "a generated marker line is missing or altered",
            Self::Purpose => "purpose slot is malformed or oversize",
            Self::GeneratedEdited => "a generated section was edited",
            Self::Anchor => "a link destination could not be decoded",
            Self::ManagedIgnore => "managed ignore markers are duplicated or malformed",
            Self::Unreadable => "file could not be read",
        }
    }
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One path that needs a human before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    path: String,
    reason: ConflictReason,
}

impl Conflict {
    pub(crate) const fn new(path: String, reason: ConflictReason) -> Self {
        Self { path, reason }
    }

    /// The repository-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn reason(&self) -> ConflictReason {
        self.reason
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

/// The full comparison of one repository's text artifacts against a snapshot.
#[derive(Debug, Clone, Default)]
pub struct TextValidation {
    fresh: Vec<String>,
    stale: Vec<String>,
    missing: Vec<String>,
    removable: Vec<String>,
    conflicts: Vec<Conflict>,
    over_budget: Vec<String>,
    pending_purposes: u32,
    index_hash: Option<Digest>,
}

impl TextValidation {
    /// Artifacts already byte-identical to the generation.
    #[must_use]
    pub fn fresh(&self) -> &[String] {
        &self.fresh
    }

    /// Owned artifacts that describe an older projection.
    #[must_use]
    pub fn stale(&self) -> &[String] {
        &self.stale
    }

    /// Artifacts the generation would create.
    #[must_use]
    pub fn missing(&self) -> &[String] {
        &self.missing
    }

    /// Owned artifacts the new plan no longer has a use for.
    #[must_use]
    pub fn removable(&self) -> &[String] {
        &self.removable
    }

    /// Every path that needs attention, in canonical path order.
    #[must_use]
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    /// Scopes holding one indivisible record wider than the budget.
    #[must_use]
    pub fn over_budget(&self) -> &[String] {
        &self.over_budget
    }

    /// Routers whose purpose is still the generated placeholder.
    #[must_use]
    pub const fn pending_purposes(&self) -> u32 {
        self.pending_purposes
    }

    /// The identity every artifact of this generation carries.
    #[must_use]
    pub const fn index_hash(&self) -> Option<Digest> {
        self.index_hash
    }

    /// Whether the repository can be published without human intervention.
    #[must_use]
    pub fn is_publishable(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// Whether every artifact already matches the snapshot.
    ///
    /// This is what lets `rr refresh` touch nothing on a clean repository.
    #[must_use]
    pub fn is_up_to_date(&self) -> bool {
        self.conflicts.is_empty()
            && self.stale.is_empty()
            && self.missing.is_empty()
            && self.removable.is_empty()
    }
}

/// Compares every text artifact in `root` against `snapshot`.
///
/// # Errors
/// Returns an error only for failures that make the comparison itself
/// impossible — an unprojectable snapshot or an unusable budget. A file that
/// cannot be read or parsed becomes a [`Conflict`], because the purpose of this
/// function is to report those rather than to stop at one.
pub fn validate_text_artifacts(
    snapshot: &Snapshot,
    root: &Path,
    budget: u32,
) -> crate::Result<TextValidation> {
    let projection = TextProjection::from_snapshot(snapshot, budget)?;
    let purposes = read_existing_purposes(root, &projection)?;
    let rendered = projection.render(&purposes)?;
    Ok(compare(root, &projection, &rendered))
}

/// The comparison, split out so it can be tested without a snapshot.
fn compare(
    root: &Path,
    projection: &TextProjection,
    rendered: &RenderedArtifactSet,
) -> TextValidation {
    let mut validation = TextValidation {
        pending_purposes: rendered.pending_purposes(),
        index_hash: Some(rendered.index_hash()),
        over_budget: projection.over_budget_scopes().map(str::to_owned).collect(),
        ..TextValidation::default()
    };

    let planned: BTreeSet<&str> = rendered.committed_paths().collect();
    for file in rendered.files() {
        classify(
            root,
            file.path(),
            file.bytes(),
            file.kind(),
            &mut validation,
        );
    }

    // A directory that lost its last source file leaves a valid, owned map
    // behind. It is only removable while it still validates: a page somebody
    // edited is evidence of intent, and intent outranks a stale plan. The
    // reasons are the same ones a planned artifact gets, because a human
    // deciding what to do about the file has the same two questions.
    for path in owned_paths_on_disk(root, &planned) {
        match read_and_classify_existing(root, &path) {
            Ok(true) => validation.removable.push(path),
            Ok(false) => validation
                .conflicts
                .push(Conflict::new(path, ConflictReason::GeneratedEdited)),
            Err(reason) => validation.conflicts.push(Conflict::new(path, reason)),
        }
    }

    validation.conflicts.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| left.reason.as_str().cmp(right.reason.as_str()))
    });
    validation.removable.sort();
    validation
}

/// Decides one planned artifact's state.
fn classify(
    root: &Path,
    path: &str,
    expected: &[u8],
    kind: ArtifactKind,
    validation: &mut TextValidation,
) {
    let absolute = root.join(path);
    let actual = match std::fs::read(&absolute) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validation.missing.push(path.to_owned());
            return;
        }
        Err(_) => {
            validation
                .conflicts
                .push(Conflict::new(path.to_owned(), ConflictReason::Unreadable));
            return;
        }
    };
    if actual == expected {
        validation.fresh.push(path.to_owned());
        return;
    }
    let outcome = ownership_of(&actual, kind);
    if outcome == Ok(true) || repairs_in_place(kind, outcome) {
        validation.stale.push(path.to_owned());
        return;
    }
    let reason = match outcome {
        Ok(_) => ConflictReason::GeneratedEdited,
        Err(reason) => reason,
    };
    validation
        .conflicts
        .push(Conflict::new(path.to_owned(), reason));
}

/// Whether a damaged artifact of this kind is rewritten rather than reported.
///
/// `.rr/SYMBOLS.md` is declared fully replaceable: it holds nothing a human
/// authored, so a copy rr wrote and something later damaged is repaired in
/// place instead of stopping the run. That licence covers damage *inside* an rr
/// artifact only. A file that never claimed to be rr's, or one a merge left
/// conflicted, is somebody else's work sitting at a reserved path — it is
/// reported, never overwritten.
fn repairs_in_place(kind: ArtifactKind, outcome: Result<bool, ConflictReason>) -> bool {
    kind == ArtifactKind::Symbols
        && !matches!(
            outcome,
            Err(ConflictReason::NotOwned | ConflictReason::MergeConflict)
        )
}

/// Whether these bytes even claim to be an artifact of this crate.
///
/// Asked before any grammar question. A hand-written file at a reserved path is
/// somebody else's file, and telling its author that their frontmatter is not
/// the supported subset describes a document they never wrote — the answer they
/// need is that the path is taken.
fn declares_rr_artifact(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(rest) = text.strip_prefix("---\n") else {
        return false;
    };
    rest.starts_with("type: \"rr-")
}

/// Whether these bytes are an rr-owned artifact of the expected kind.
///
/// Three questions in the one order that gives a human an answer they can act
/// on: is this a conflicted file, is it ours at all, and only then, is it
/// intact. Reversing the first two reports a merge as a path collision.
fn ownership_of(bytes: &[u8], kind: ArtifactKind) -> Result<bool, ConflictReason> {
    if std::str::from_utf8(bytes).is_ok_and(super::parse::has_conflict_markers) {
        return Err(ConflictReason::MergeConflict);
    }
    if !declares_rr_artifact(bytes) {
        return Err(ConflictReason::NotOwned);
    }
    match kind {
        ArtifactKind::Symbols => parse_symbols(bytes)
            .map(|parsed| parsed.is_owned())
            .map_err(conflict_reason_of),
        ArtifactKind::Router | ArtifactKind::Page => parse_map(bytes)
            .map(|parsed| parsed.is_owned())
            .map_err(conflict_reason_of),
    }
}

/// Maps a parse failure onto the reason a human is shown.
fn conflict_reason_of(error: crate::Error) -> ConflictReason {
    let crate::Error::Text(error) = error else {
        return ConflictReason::Unreadable;
    };
    match error {
        super::TextError::MergeConflict => ConflictReason::MergeConflict,
        super::TextError::Marker { .. } => ConflictReason::Marker,
        super::TextError::Purpose { .. } => ConflictReason::Purpose,
        super::TextError::UnsupportedFormat { .. } => ConflictReason::UnsupportedFormat,
        super::TextError::Destination { .. } => ConflictReason::Anchor,
        super::TextError::ManagedIgnore { .. } => ConflictReason::ManagedIgnore,
        super::TextError::NotUtf8 | super::TextError::Newline { .. } => ConflictReason::NotOwned,
        _ => ConflictReason::Frontmatter,
    }
}

/// Reserved paths that exist on disk but are not in the new plan.
///
/// Only directories the plan already knows about are scanned. A repository-wide
/// walk here would be a second discovery pass with its own idea of what to
/// ignore, and the snapshot has already answered that question.
fn owned_paths_on_disk(root: &Path, planned: &BTreeSet<&str>) -> Vec<String> {
    let mut directories: BTreeSet<&str> = BTreeSet::new();
    for path in planned {
        directories.insert(path.rsplit_once('/').map_or("", |(head, _)| head));
    }

    let mut found = Vec::new();
    for directory in directories {
        let absolute = if directory.is_empty() {
            root.to_path_buf()
        } else {
            root.join(directory)
        };
        let Ok(entries) = std::fs::read_dir(&absolute) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !name.starts_with(super::OVERFLOW_PREFIX)
                || !name.ends_with(super::MARKDOWN_EXTENSION)
            {
                continue;
            }
            let path = if directory.is_empty() {
                name
            } else {
                format!("{directory}/{name}")
            };
            if !planned.contains(path.as_str()) {
                found.push(path);
            }
        }
    }
    found
}

/// Whether a stale reserved file may be removed.
///
/// `Ok(true)` means an rr-owned page that still validates, `Ok(false)` an rr
/// page somebody edited, and an error names what stopped it being either.
fn read_and_classify_existing(root: &Path, path: &str) -> Result<bool, ConflictReason> {
    let bytes = std::fs::read(root.join(path)).map_err(|_| ConflictReason::Unreadable)?;
    ownership_of(&bytes, ArtifactKind::Page)
}

/// Which committed map owns each symbol, and at what API identity.
///
/// Issue #12 stores exactly this pair against a learned route. It deliberately
/// does not expose `generated_hash`: a route invalidated by somebody rewording
/// a purpose would be a route that never survives a day.
#[derive(Debug, Clone)]
pub struct MapCatalog {
    owners: BTreeMap<SymbolId, MapIdentity>,
    index_hash: Digest,
}

/// One committed map and the API identity of its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapIdentity {
    path: RelPath,
    api_hash: ApiHash,
}

impl MapIdentity {
    /// The canonical repository-relative path of the owning map.
    #[must_use]
    pub const fn path(&self) -> &RelPath {
        &self.path
    }

    /// The scope's API identity, which is the invalidation key.
    #[must_use]
    pub const fn api_hash(&self) -> ApiHash {
        self.api_hash
    }
}

impl MapCatalog {
    /// The map that lists this symbol, if any does.
    #[must_use]
    pub fn owner(&self, symbol: SymbolId) -> Option<&MapIdentity> {
        self.owners.get(&symbol)
    }

    /// The projection this catalog was built from.
    #[must_use]
    pub const fn index_hash(&self) -> Digest {
        self.index_hash
    }

    /// How many symbols have an owning map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.owners.len()
    }

    /// Whether no symbol has an owning map.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }
}

/// Builds the catalog, but only from artifacts that are actually valid on disk.
///
/// The validation is the point. A catalog built from a projection alone would
/// name maps that may not exist, and issue #12 would then learn routes to files
/// no reader can open.
///
/// # Errors
/// Returns an error when the snapshot cannot be projected, and
/// [`crate::Error::Text`] with [`super::TextError::IndexHashMismatch`] when the
/// artifacts on disk do not agree with the snapshot — the case a caller must
/// repair before trusting any route.
pub fn validated_map_catalog(
    snapshot: &Snapshot,
    root: &Path,
    budget: u32,
) -> crate::Result<MapCatalog> {
    let projection = TextProjection::from_snapshot(snapshot, budget)?;
    let validation = validate_text_artifacts(snapshot, root, budget)?;
    if !validation.is_up_to_date() {
        return Err(crate::Error::Text(super::TextError::IndexHashMismatch));
    }

    let mut owners = BTreeMap::new();
    for scope in projection.scopes() {
        let Ok(path) = RelPath::new(scope.path.map_path()) else {
            continue;
        };
        for record in &scope.api {
            owners.insert(
                record.symbol,
                MapIdentity {
                    path: path.clone(),
                    api_hash: scope.api_hash,
                },
            );
        }
    }
    Ok(MapCatalog {
        owners,
        index_hash: projection.index_hash(),
    })
}

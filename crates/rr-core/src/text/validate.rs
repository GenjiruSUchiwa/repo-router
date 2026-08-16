//! What is on disk, compared against what the snapshot says should be.
//!
//! One question per artifact — fresh, stale, missing, removable, or
//! conflicting — and no action on the answer: acting belongs to whoever holds
//! the publication guard. The answer is five lists on [`TextValidation`] rather
//! than one label per file, because a caller acts on the *set*: it writes the
//! stale ones, unlinks the removable ones, and stops on any conflict.
//!
//! Conflicts are collected, not thrown, so one run names every file that needs
//! attention instead of one per run.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use crate::index::{Snapshot, SymbolId};
use crate::path::RelPath;

use super::digest::{ApiHash, Digest};
use super::model::TextProjection;
use super::purpose::read_existing_purposes;
use super::{parse_map, parse_symbols, ArtifactKind, RenderedArtifactSet, RenderedFile};

/// Why one path cannot be written.
///
/// A closed set rather than a string, because the CLI prints these and issue
/// #14 will branch on them. A message is a description; this is a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictReason {
    /// A file exists at a reserved path that rr did not write.
    NotOwned,
    /// The reserved path is a symbolic link.
    Symlink,
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
    /// The filesystem resolves this path to a file spelled differently.
    CaseCollision,
}

impl ConflictReason {
    /// The published spelling, identical to the serde name.
    ///
    /// Text output cannot go through serde, so the two spellings are pinned
    /// together by a test rather than by construction.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotOwned => "not-owned",
            Self::Symlink => "symlink",
            Self::MergeConflict => "merge-conflict",
            Self::Frontmatter => "frontmatter",
            Self::UnsupportedFormat => "unsupported-format",
            Self::Marker => "marker",
            Self::Purpose => "purpose",
            Self::GeneratedEdited => "generated-edited",
            Self::Anchor => "anchor",
            Self::ManagedIgnore => "managed-ignore",
            Self::Unreadable => "unreadable",
            Self::CaseCollision => "case-collision",
        }
    }

    /// The one-line explanation the CLI prints after the path.
    #[must_use]
    pub const fn as_text(self) -> &'static str {
        match self {
            Self::NotOwned => "path is not owned by rr",
            Self::Symlink => "path is a symbolic link; rr writes only regular files",
            Self::MergeConflict => "file contains Git conflict markers",
            Self::Frontmatter => "frontmatter is not the supported format",
            Self::UnsupportedFormat => "file declares an unsupported format version",
            Self::Marker => "a generated marker line is missing or altered",
            Self::Purpose => "purpose slot is malformed or oversize",
            Self::GeneratedEdited => "a generated section was edited",
            Self::Anchor => "a link destination could not be decoded",
            Self::ManagedIgnore => "managed ignore markers are duplicated or malformed",
            Self::Unreadable => "file could not be read",
            Self::CaseCollision => {
                "the filesystem already holds this path under a different spelling"
            }
        }
    }
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_text())
    }
}

/// One path that needs a human before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    path: String,
    reason: ConflictReason,
    found: Option<String>,
}

impl Conflict {
    pub(crate) const fn new(path: String, reason: ConflictReason) -> Self {
        Self {
            path,
            reason,
            found: None,
        }
    }

    /// A path the filesystem resolves to a differently-spelled file.
    ///
    /// Both names are kept because neither alone is actionable: the planned one
    /// is what rr will not write, and `found` is the only one the user's shell
    /// will match.
    pub(crate) const fn colliding(path: String, found: String) -> Self {
        Self {
            path,
            reason: ConflictReason::CaseCollision,
            found: Some(found),
        }
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

    /// The differently-spelled file this path resolves to, when there is one.
    #[must_use]
    pub fn found(&self) -> Option<&str> {
        self.found.as_deref()
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.found {
            Some(found) => write!(f, "{}: {} ({found})", self.path, self.reason),
            None => write!(f, "{}: {}", self.path, self.reason),
        }
    }
}

/// Serialized as `{path, reason, message}`.
///
/// `reason` is what a program branches on — the closed set this enum exists to
/// be. `message` is the sentence a human reads, and it is here because under
/// `--json` it is printed nowhere else: the refusal object replaces output that
/// used to be nothing but that sentence. Derived from `reason` in one place, so
/// the two cannot drift.
impl serde::Serialize for Conflict {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;

        let mut object = serializer.serialize_struct("Conflict", 3)?;
        object.serialize_field("path", &self.path)?;
        object.serialize_field("reason", &self.reason)?;
        object.serialize_field("message", self.reason.as_text())?;
        object.end()
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
    symbols_repaired: bool,
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

    /// Scopes no page of which fits the budget.
    #[must_use]
    pub fn over_budget(&self) -> &[String] {
        &self.over_budget
    }

    /// Routers whose purpose is still the generated placeholder.
    #[must_use]
    pub const fn pending_purposes(&self) -> u32 {
        self.pending_purposes
    }

    /// Whether the local symbol index had to be rewritten because it was no
    /// longer valid, rather than merely because it was out of date.
    #[must_use]
    pub const fn symbols_repaired(&self) -> bool {
        self.symbols_repaired
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
    Ok(stage_text_artifacts(snapshot, root, budget)?.validation)
}

/// One generation's bytes together with what the repository currently holds.
///
/// A publisher needs both and they must come from the same projection, so they
/// are produced together rather than by two calls that could see two snapshots.
#[derive(Debug)]
pub struct StagedText {
    rendered: RenderedArtifactSet,
    validation: TextValidation,
}

impl StagedText {
    /// The bytes this generation would publish.
    #[must_use]
    pub const fn rendered(&self) -> &RenderedArtifactSet {
        &self.rendered
    }

    /// What is on disk, compared against those bytes.
    #[must_use]
    pub const fn validation(&self) -> &TextValidation {
        &self.validation
    }
}

/// Renders one generation and compares it against `root` in a single pass.
///
/// # Errors
/// As [`validate_text_artifacts`].
pub fn stage_text_artifacts(
    snapshot: &Snapshot,
    root: &Path,
    budget: u32,
) -> crate::Result<StagedText> {
    let projection = TextProjection::from_snapshot(snapshot, budget)?;
    let purposes = read_existing_purposes(root, &projection)?;
    let rendered = projection.render(&purposes)?;
    let validation = compare(root, &projection, &rendered);
    Ok(StagedText {
        rendered,
        validation,
    })
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
    let collisions = case_collisions(
        root,
        rendered
            .files()
            .iter()
            .map(RenderedFile::path)
            .chain(std::iter::once(super::IGNORE_PATH)),
    );
    for file in rendered.files() {
        classify(
            root,
            file.path(),
            file.bytes(),
            file.kind(),
            &collisions,
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

    classify_managed_ignore(root, &collisions, &mut validation);

    validation.conflicts.sort_by(|left, right| {
        left.path
            .as_bytes()
            .cmp(right.path.as_bytes())
            .then_with(|| left.reason.as_str().cmp(right.reason.as_str()))
    });
    validation.removable.sort();
    validation
}

/// Reports `.rr/.gitignore` when a human has to resolve it, and only then.
///
/// Asked here because the answer *can* be "resolve this first", and a publisher
/// that discovered that while writing would already have replaced the snapshot
/// and every map — the one thing staging exists to prevent. Whether the block
/// is merely absent or out of date is deliberately not reported: this module
/// never writes that file, so making it part of "up to date" would let the
/// answer depend on a caller that may not exist.
fn classify_managed_ignore(
    root: &Path,
    collisions: &BTreeMap<String, String>,
    validation: &mut TextValidation,
) {
    let path = super::IGNORE_PATH;
    if let Some(found) = collisions.get(path) {
        validation
            .conflicts
            .push(Conflict::colliding(path.to_owned(), found.clone()));
        return;
    }
    let existing = match std::fs::read_to_string(root.join(path)) {
        Ok(text) => text,
        // Absent is not a problem to report: the block is appended on the next
        // write, and until then the directory is excluded by name anyway.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            validation
                .conflicts
                .push(Conflict::new(path.to_owned(), ConflictReason::Unreadable));
            return;
        }
    };
    if super::apply_managed_block(Some(&existing)).is_err() {
        validation.conflicts.push(Conflict::new(
            path.to_owned(),
            ConflictReason::ManagedIgnore,
        ));
    }
}

/// Decides one planned artifact's state.
fn classify(
    root: &Path,
    path: &str,
    expected: &[u8],
    kind: ArtifactKind,
    collisions: &BTreeMap<String, String>,
    validation: &mut TextValidation,
) {
    if let Some(found) = collisions.get(path) {
        validation
            .conflicts
            .push(Conflict::colliding(path.to_owned(), found.clone()));
        return;
    }

    let absolute = root.join(path);
    // Before reading, because every read here follows the link: a symlink would
    // be judged on its target's bytes and then written through, putting rr's
    // output somewhere rr never chose.
    if std::fs::symlink_metadata(&absolute).is_ok_and(|meta| meta.file_type().is_symlink()) {
        validation
            .conflicts
            .push(Conflict::new(path.to_owned(), ConflictReason::Symlink));
        return;
    }
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
    if outcome == Ok(true) {
        validation.stale.push(path.to_owned());
        return;
    }
    if repairs_in_place(kind, outcome) {
        // Distinguished from the line above because the two are different
        // events: one is a file that says something older, the other is a file
        // that no longer says anything valid. Only the second is a repair.
        validation.symbols_repaired = true;
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

/// The planned artifacts whose path resolves to a differently-spelled file.
///
/// One `read_dir` per planned directory rather than one metadata call per
/// planned file, because on a case-insensitive filesystem the metadata call
/// succeeds under the wrong name — which is the whole failure. Only the
/// directory listing knows how a file is really spelled.
///
/// Asks nothing about the filesystem's type, and does not have to: the listing
/// finds the candidate, and one call under the planned name settles whether the
/// volume folds. On a case-sensitive one the two spellings are two files, the
/// planned one is simply absent, and there is no collision to report. ASCII
/// folding is exact because every name this module plans is ASCII.
fn case_collisions<'a>(
    root: &Path,
    planned: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let mut by_directory: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in planned {
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        by_directory.entry(directory).or_default().push(name);
    }

    let mut collisions = BTreeMap::new();
    for (directory, names) in by_directory {
        let absolute = if directory.is_empty() {
            root.to_path_buf()
        } else {
            root.join(directory)
        };
        // A directory that is not there yet holds nothing to collide with, and
        // rr is about to create it.
        let Ok(entries) = std::fs::read_dir(&absolute) else {
            continue;
        };
        let present: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        for name in names {
            if present.iter().any(|entry| entry == name) {
                continue;
            }
            if let Some(found) = present
                .iter()
                .find(|entry| entry.eq_ignore_ascii_case(name))
            {
                // Folding is what makes this a collision rather than two files.
                // The planned name is not in the listing, so on a
                // case-sensitive volume nothing resolves under it and rr is
                // free to create it beside `found`. Only a filesystem that
                // folds case answers this call, and that answer is `found`.
                if absolute.join(name).symlink_metadata().is_err() {
                    continue;
                }
                collisions.insert(join(directory, name), join(directory, found));
            }
        }
    }
    collisions
}

/// Rejoins a directory and a name into a repository-relative path.
fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// Whether a damaged artifact of this kind is rewritten rather than reported.
///
/// `.rr/SYMBOLS.md` is fully replaceable, so rr repairs its own damaged copy.
/// A foreign or merge-conflicted file there is still reported, never seized.
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
/// Follows the committed generation rather than walking the repository: the
/// scan starts at the root and descends only into directories that hold a
/// `MAP.md`. rr writes one in every indexed directory and in every ancestor of
/// one, so that set is connected and rooted at the repository — which finds
/// every artifact rr has written here and stops at the first directory it never
/// touched, without a second opinion about what to ignore.
///
/// Routers count, not only overflow pages. A directory that loses its last
/// source file leaves a committed map that nothing links to and no later run
/// would ever revisit.
fn owned_paths_on_disk(root: &Path, planned: &BTreeSet<&str>) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];

    while let Some(directory) = pending.pop() {
        let absolute = if directory.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&directory)
        };
        let Ok(entries) = std::fs::read_dir(&absolute) else {
            continue;
        };

        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let path = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };

            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {
                    if absolute.join(&name).join(super::MAP_FILE_NAME).is_file() {
                        pending.push(path);
                    }
                }
                Ok(_) => {
                    if super::is_reserved_artifact_name(&name) && !planned.contains(path.as_str()) {
                        found.push(path);
                    }
                }
                Err(_) => {}
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
/// A catalog built from a projection alone would name maps that may not exist,
/// leaving issue #12 with routes to files no reader can open.
///
/// # Errors
/// When the snapshot cannot be projected, and [`super::TextError::IndexHashMismatch`]
/// when the artifacts on disk disagree with it — repair before trusting a route.
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

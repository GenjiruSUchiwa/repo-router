//! The two endpoints one impact run compares, and the deltas between them.
//!
//! Git diffs *endpoints*, not commit sets: this takes two resolved endpoints
//! and invents no range syntax, so no CLI reimplements the `A..B`/`A...B`
//! distinction and gets it wrong. A caller names each side once, this module
//! resolves it once, and every delta below is a statement about those two
//! resolutions and nothing else.
//!
//! Two guarantees hold across every path through here, because everything
//! downstream reads line ranges as definition boundaries. Hunks carry zero
//! context, so a range names only lines the edit replaced. And a file whose
//! bytes moved between the observation and the read is reported as raced rather
//! than described: a delta computed from two different versions of one file is
//! not a delta of anything.

use std::collections::{BTreeMap, BTreeSet};

use gix::bstr::{BStr, ByteSlice};
use gix::objs::tree::EntryMode;

use rr_core::cancel::CancelToken;
use rr_core::path::RelPath;
use rr_core::verify::{ContentPathState, Revalidation, MAX_SOURCE_BYTES};
use rr_core::walk::{collected_lang, WalkCfg};

use crate::content::AcquiredContent;
use crate::content::{acquire_for_source, object_id, revalidate_source, AcquireOutcome};
use crate::oid::Oid;
use crate::repo::{ChangeKind, GitRepo, HeadState, RepoState};
use crate::{Error, Result};

/// One side of a comparison, resolved to bytes that cannot move underneath us.
///
/// `Tree` keeps the caller's spelling beside the oid: two runs naming `HEAD` at
/// different times are different reports, and one that recorded only the word
/// `HEAD` could never say which of the two it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeTarget {
    /// A committed tree, pinned to the commit one revision spelling resolved to.
    Tree {
        /// The revision exactly as the caller wrote it.
        spec: String,
        /// The commit `spec` resolved to, and therefore the tree compared.
        commit: Oid,
    },
    /// Index, unstaged tracked edits and eligible untracked sources, seen once.
    Worktree {
        /// Where `HEAD` pointed when this endpoint was resolved.
        ///
        /// A working tree is not addressable by a hash, so this is the only
        /// identity it has: the same edits on top of a different commit are a
        /// different endpoint, and a report that dropped this could not say so.
        head: HeadState,
    },
}

/// One file's difference between the two endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDelta {
    /// Reuses [`crate::repo::ChangeKind`] so status and diff cannot disagree.
    pub kind: ChangeKind,
    /// New-side path; for [`ChangeKind::Deleted`], the path that went away.
    pub path: RelPath,
    /// Old-side path of a rename or copy.
    pub source: Option<RelPath>,
    /// Blob the base endpoint holds; absent for an addition.
    pub base_oid: Option<Oid>,
    /// Blob the target endpoint holds.
    ///
    /// Absent for a deletion, and absent for the rarer case of a target entry
    /// this comparison cannot identify at all: a path that stopped being a
    /// regular file, or one past the acquisition cap. Both mean the same thing
    /// to a reader — there are no target bytes — and both come with no hunks,
    /// which is how a whole-file change is spelled here.
    pub target_oid: Option<Oid>,
    /// Zero-context hunks, ordered by `new_start` then `old_start`.
    ///
    /// Empty means *whole file changed*, never *nothing changed*: an addition, a
    /// deletion, bytes no line diff can address, or a rename that moved the
    /// exact same bytes.
    pub hunks: Vec<Hunk>,
}

/// One `-U0` hunk: the half-open line ranges it replaces on each side.
///
/// Lines are 1-based, so a pure insertion is `old_lines == 0`. A side with no
/// lines reports the line the change *follows*, exactly as `git diff -U0`
/// prints it, so `0` there means "before the first line". Context is excluded —
/// three context lines intersect three untouched definitions, three false
/// `affected` entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hunk {
    /// First base-side line the hunk replaces, or the line it follows.
    pub old_start: u32,
    /// Base-side lines replaced; `0` for a pure insertion.
    pub old_lines: u32,
    /// First target-side line the hunk introduces, or the line it follows.
    pub new_start: u32,
    /// Target-side lines introduced; `0` for a pure deletion.
    pub new_lines: u32,
}

/// Everything one comparison observed, including what it could not trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// The endpoint the comparison started from.
    pub base: ChangeTarget,
    /// The endpoint the comparison ended at.
    pub target: ChangeTarget,
    /// Sorted by `path`; one entry per changed path.
    pub files: Vec<FileDelta>,
    /// Bytes changed between the observation and the read.
    ///
    /// A raced path has no [`FileDelta`], because the only delta available for
    /// it would mix two endpoints. Listing it keeps the omission visible: a
    /// caller reports a partial run rather than a complete one that quietly
    /// skipped a file.
    pub raced: Vec<RelPath>,
    /// Unmerged index stages.
    ///
    /// A conflicted path has no [`FileDelta`] either: it has two or three
    /// competing base sides, and picking one would be this module inventing the
    /// merge resolution the user has not made yet.
    pub conflicted: Vec<RelPath>,
}

/// Resolves one revision spelling into a committed-tree endpoint.
///
/// The spelling is kept beside the commit it resolved to, so a report can say
/// which words produced the oids it compared. Only single revisions resolve
/// here: a range spelling is rejected rather than reinterpreted, which is what
/// keeps the `A..B`/`A...B` distinction out of this crate entirely.
///
/// # Errors
/// Returns [`Error::Content`] when `spec` names nothing this repository can
/// peel to a commit — an unknown name, a range, or an object that is not a
/// commit — and [`Error::Oid`] if a resolved commit id is not a valid oid.
pub fn resolve_target(repo: &GitRepo, spec: &str) -> Result<ChangeTarget> {
    let unresolvable = || Error::Content(format!("unresolvable revision: {spec}"));
    let id = repo
        .gix_repo()
        .rev_parse_single(spec)
        .map_err(|_| unresolvable())?;
    let commit = id
        .object()
        .map_err(|_| unresolvable())?
        .peel_to_commit()
        .map_err(|_| unresolvable())?;
    Ok(ChangeTarget::Tree {
        spec: spec.to_owned(),
        commit: Oid::from_raw(commit.id.as_bytes())?,
    })
}

/// Resolves the working tree as a comparison endpoint.
///
/// # Errors
/// Returns [`Error::Content`] when `HEAD` cannot be inspected.
pub fn worktree_target(repo: &GitRepo) -> Result<ChangeTarget> {
    Ok(ChangeTarget::Worktree {
        head: repo
            .head_oid()?
            .map_or(HeadState::Unborn, HeadState::Commit),
    })
}

/// Everything `base` and `target` disagree about.
///
/// `walk` decides which *untracked* files count as sources, through
/// [`collected_lang`] — the same answer discovery gives, so impact and map
/// cannot disagree about what a source file is. Tracked paths are reported
/// whatever their name, because a changed tracked file is a fact about the
/// repository even when no extractor will ever read it.
///
/// # Errors
/// Returns [`Error::Content`] when Git cannot produce the comparison,
/// [`Error::Cancelled`] when cancellation was requested mid-scan, and the
/// I/O and object errors of reading the two endpoints.
pub fn change_set(
    repo: &GitRepo,
    base: &ChangeTarget,
    target: &ChangeTarget,
    walk: &WalkCfg,
    cancel: &CancelToken,
) -> Result<ChangeSet> {
    let observed = match (base, target) {
        (ChangeTarget::Tree { commit: base, .. }, ChangeTarget::Tree { commit: target, .. }) => {
            Observed {
                files: tree_to_tree(repo, *base, *target, cancel)?,
                raced: Vec::new(),
                conflicted: Vec::new(),
            }
        }
        (ChangeTarget::Tree { commit: base, .. }, ChangeTarget::Worktree { .. }) => {
            tree_to_worktree(repo, *base, walk, cancel)?
        }
        (ChangeTarget::Worktree { .. }, _) => {
            return Err(Error::Content(
                "the working tree is only ever the target endpoint of a comparison".to_owned(),
            ))
        }
    };
    Ok(ChangeSet {
        base: base.clone(),
        target: target.clone(),
        files: observed.files,
        raced: observed.raced,
        conflicted: observed.conflicted,
    })
}

/// The three lists one comparison produces, before they are named as endpoints.
struct Observed {
    files: Vec<FileDelta>,
    raced: Vec<RelPath>,
    conflicted: Vec<RelPath>,
}

/// The paths a working-tree comparison will ask about, keyed so the answer is
/// derived once per path however many status items mentioned it.
struct Candidates {
    paths: BTreeSet<RelPath>,
    renames: BTreeMap<RelPath, (ChangeKind, RelPath)>,
}

/// What one candidate path contributed to the comparison.
enum PathOutcome {
    /// A difference between the two endpoints.
    Delta(FileDelta),
    /// Bytes that moved mid-read, so no delta describes them.
    Raced(RelPath),
    /// A path status mentioned that the two endpoints agree about, which
    /// happens whenever an edit is reverted or only a mode changed.
    Unchanged,
}

/// What the target endpoint holds for one path, after it has been read twice.
enum TargetSide {
    /// Canonical bytes and the one oid that names them, confirmed still current.
    Present(AcquiredContent),
    /// The path is gone from the working tree.
    Absent,
    /// The path is there and is not a regular file: a symlink, a directory, a
    /// device. It has no source bytes, and its kind is what changed.
    NotAFile,
    /// A regular file past the acquisition cap. Its content differs, and no
    /// line ranges can be offered for it.
    Oversize,
    /// The bytes moved between the observation and the read.
    Raced,
}

/// Every path two committed trees disagree about, with their hunks.
fn tree_to_tree(
    repo: &GitRepo,
    base: Oid,
    target: Oid,
    cancel: &CancelToken,
) -> Result<Vec<FileDelta>> {
    let mut files = tree_deltas(repo, base, Some(target), cancel)?;
    for delta in &mut files {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        delta.hunks = committed_hunks(repo, delta)?;
    }
    Ok(files)
}

/// Every path one committed tree and the working tree disagree about.
///
/// The candidate paths come from two places, and both are needed. The working
/// tree's own differences come from [`GitRepo::observe_state`], the one
/// observation this crate makes of `HEAD`, the index and the tree at once. When
/// `base` is not `HEAD`, the paths the two commits disagree about are candidates
/// too: a file committed since `base` and never touched afterwards differs from
/// `base` while `git status` reports nothing about it at all.
///
/// Each candidate's kind is then derived from what the two endpoints actually
/// hold rather than copied from the status item, because a status kind is
/// relative to `HEAD` and this comparison may not be.
fn tree_to_worktree(
    repo: &GitRepo,
    base: Oid,
    walk: &WalkCfg,
    cancel: &CancelToken,
) -> Result<Observed> {
    let state = repo.observe_state(cancel)?;
    let conflicted: BTreeSet<RelPath> = state.conflicted.iter().cloned().collect();
    let mut candidates = worktree_candidates(&state, walk, &conflicted);

    let head = state.head.commit();
    if head != Some(base) {
        for delta in tree_deltas(repo, base, head, cancel)? {
            if !conflicted.contains(&delta.path) {
                candidates.paths.insert(delta.path);
            }
            if let Some(source) = delta.source.filter(|source| !conflicted.contains(source)) {
                candidates.paths.insert(source);
            }
        }
    }
    for (kind, source) in candidates.renames.values() {
        if *kind == ChangeKind::Renamed {
            candidates.paths.remove(source);
        }
    }

    let base_tree = tree_of(repo, base)?;
    let mut files = Vec::new();
    let mut raced = Vec::new();
    for path in candidates.paths {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let rename = candidates.renames.get(&path);
        match worktree_delta(repo, &base_tree, path, rename)? {
            PathOutcome::Delta(delta) => files.push(delta),
            PathOutcome::Raced(path) => raced.push(path),
            PathOutcome::Unchanged => {}
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Observed {
        files,
        raced,
        conflicted: state.conflicted,
    })
}

/// The paths one working-tree comparison must ask about, and the renames Git
/// already knows the source of.
///
/// Conflicted paths are dropped here rather than filtered later: they have no
/// single base side, and asking about them at all would end in a delta against
/// whichever stage happened to be read.
fn worktree_candidates(
    state: &RepoState,
    walk: &WalkCfg,
    conflicted: &BTreeSet<RelPath>,
) -> Candidates {
    let mut candidates = Candidates {
        paths: BTreeSet::new(),
        renames: BTreeMap::new(),
    };
    for change in &state.changes {
        if conflicted.contains(&change.path) {
            continue;
        }
        if change.kind == ChangeKind::Untracked
            && collected_lang(change.path.as_str(), walk).is_none()
        {
            continue;
        }
        if let (ChangeKind::Renamed | ChangeKind::Copied, Some(source)) =
            (change.kind, change.source.as_ref())
        {
            candidates
                .renames
                .insert(change.path.clone(), (change.kind, source.clone()));
        }
        candidates.paths.insert(change.path.clone());
    }
    candidates
}

/// What the two endpoints hold for one candidate path.
fn worktree_delta(
    repo: &GitRepo,
    base_tree: &gix::Tree<'_>,
    path: RelPath,
    rename: Option<&(ChangeKind, RelPath)>,
) -> Result<PathOutcome> {
    let base_oid = tree_blob(base_tree, rename.map_or(&path, |(_, source)| source))?;
    let source = rename.map(|(_, source)| source.clone());
    let whole_file = |kind: ChangeKind| FileDelta {
        kind,
        path: path.clone(),
        source: source.clone(),
        base_oid,
        target_oid: None,
        hunks: Vec::new(),
    };

    let outcome = match read_target(repo, &path)? {
        TargetSide::Raced => PathOutcome::Raced(path),
        TargetSide::Absent | TargetSide::NotAFile if base_oid.is_none() => PathOutcome::Unchanged,
        TargetSide::Absent => PathOutcome::Delta(whole_file(ChangeKind::Deleted)),
        TargetSide::NotAFile => PathOutcome::Delta(whole_file(ChangeKind::TypeChanged)),
        TargetSide::Oversize => PathOutcome::Delta(whole_file(presence_kind(base_oid, rename))),
        TargetSide::Present(content) => {
            if base_oid == Some(content.oid) && rename.is_none() {
                return Ok(PathOutcome::Unchanged);
            }
            let hunks = match base_oid {
                Some(base_oid) => worktree_hunks(repo, base_oid, &content)?,
                None => Vec::new(),
            };
            PathOutcome::Delta(FileDelta {
                kind: presence_kind(base_oid, rename),
                path,
                source,
                base_oid,
                target_oid: Some(content.oid),
                hunks,
            })
        }
    };
    Ok(outcome)
}

/// What happened to a path both endpoints can still be asked about.
///
/// A rename or copy keeps the kind Git reported for it; everything else is
/// decided by which endpoints hold the path, never by the status item, which is
/// relative to `HEAD` rather than to the base endpoint.
fn presence_kind(base_oid: Option<Oid>, rename: Option<&(ChangeKind, RelPath)>) -> ChangeKind {
    match rename {
        Some((kind, _)) => *kind,
        None if base_oid.is_some() => ChangeKind::Modified,
        None => ChangeKind::Added,
    }
}

/// Every path two trees disagree about, as deltas with no hunks yet.
///
/// Rename tracking is configured here rather than read from the repository's
/// `diff.renames`, so two clones of one repository produce the same report.
fn tree_deltas(
    repo: &GitRepo,
    base: Oid,
    target: Option<Oid>,
    cancel: &CancelToken,
) -> Result<Vec<FileDelta>> {
    let base_tree = tree_of(repo, base)?;
    let target_tree = target.map(|oid| tree_of(repo, oid)).transpose()?;
    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = repo
        .gix_repo()
        .diff_tree_to_tree(Some(&base_tree), target_tree.as_ref(), options)
        .map_err(|error| Error::Content(format!("tree comparison failed: {error}")))?;

    let mut files = Vec::with_capacity(changes.len());
    for change in &changes {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(delta) = tree_delta(change)? {
            files.push(delta);
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// One tree-level change as a delta, or `None` when it is not about a file.
fn tree_delta(change: &gix::object::tree::diff::ChangeDetached) -> Result<Option<FileDelta>> {
    use gix::object::tree::diff::ChangeDetached as Change;

    let delta = match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let Some(path) = file_path(location.as_ref(), *entry_mode) else {
                return Ok(None);
            };
            FileDelta {
                kind: ChangeKind::Added,
                path,
                source: None,
                base_oid: None,
                target_oid: Some(Oid::from_raw(id.as_bytes())?),
                hunks: Vec::new(),
            }
        }
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let Some(path) = file_path(location.as_ref(), *entry_mode) else {
                return Ok(None);
            };
            FileDelta {
                kind: ChangeKind::Deleted,
                path,
                source: None,
                base_oid: Some(Oid::from_raw(id.as_bytes())?),
                target_oid: None,
                hunks: Vec::new(),
            }
        }
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let Some(path) = file_path(location.as_ref(), *previous_entry_mode) else {
                return Ok(None);
            };
            modified_delta(
                path,
                (*previous_entry_mode, *previous_id),
                (*entry_mode, *id),
            )?
        }
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            location,
            copy,
            ..
        } => {
            let (Some(path), Some(source)) = (
                file_path(location.as_ref(), *entry_mode),
                file_path(source_location.as_ref(), *source_entry_mode),
            ) else {
                return Ok(None);
            };
            FileDelta {
                kind: if *copy {
                    ChangeKind::Copied
                } else {
                    ChangeKind::Renamed
                },
                path,
                source: Some(source),
                base_oid: Some(Oid::from_raw(source_id.as_bytes())?),
                target_oid: Some(Oid::from_raw(id.as_bytes())?),
                hunks: Vec::new(),
            }
        }
    };
    Ok(Some(delta))
}

/// One path whose entry changed, given both sides' mode and object.
///
/// A file replaced by a directory arrives here as a modification whose new mode
/// is a tree, and it is reported as a deletion: the path no longer holds
/// content, which is the half of the change any reader of this can act on.
fn modified_delta(
    path: RelPath,
    previous: (EntryMode, gix::ObjectId),
    current: (EntryMode, gix::ObjectId),
) -> Result<FileDelta> {
    let (previous_mode, previous_id) = previous;
    let (mode, id) = current;
    let base_oid = Some(Oid::from_raw(previous_id.as_bytes())?);
    if !is_file(mode) {
        return Ok(FileDelta {
            kind: ChangeKind::Deleted,
            path,
            source: None,
            base_oid,
            target_oid: None,
            hunks: Vec::new(),
        });
    }
    Ok(FileDelta {
        kind: if previous_mode.is_link() == mode.is_link() {
            ChangeKind::Modified
        } else {
            ChangeKind::TypeChanged
        },
        path,
        source: None,
        base_oid,
        target_oid: Some(Oid::from_raw(id.as_bytes())?),
        hunks: Vec::new(),
    })
}

/// The hunks one committed delta has, if line ranges can mean anything for it.
///
/// An addition and a deletion have every line on one side, which the kind
/// already says; a type change has no comparable content at all. Spelling any
/// of them as one enormous hunk would only invite a reader to intersect it with
/// definition spans that are not there.
fn committed_hunks(repo: &GitRepo, delta: &FileDelta) -> Result<Vec<Hunk>> {
    match delta.kind {
        ChangeKind::Modified | ChangeKind::Renamed | ChangeKind::Copied => {}
        ChangeKind::Added
        | ChangeKind::Deleted
        | ChangeKind::TypeChanged
        | ChangeKind::Conflicted
        | ChangeKind::Untracked => return Ok(Vec::new()),
    }
    let (Some(base), Some(target)) = (delta.base_oid, delta.target_oid) else {
        return Ok(Vec::new());
    };
    if base == target || over_diff_cap(repo, base)? || over_diff_cap(repo, target)? {
        return Ok(Vec::new());
    }
    Ok(hunks(&blob_bytes(repo, base)?, &blob_bytes(repo, target)?))
}

/// The hunks between a committed blob and bytes already read from the worktree.
fn worktree_hunks(repo: &GitRepo, base: Oid, target: &AcquiredContent) -> Result<Vec<Hunk>> {
    if base == target.oid || over_diff_cap(repo, base)? {
        return Ok(Vec::new());
    }
    Ok(hunks(&blob_bytes(repo, base)?, &target.bytes))
}

/// The zero-context hunks between two whole files.
///
/// Zero context is the point: Git's default three context lines would put up to
/// three untouched definitions inside the range of every hunk, and each one
/// would be reported as changed by whatever maps ranges to definitions.
///
/// Either side not being valid UTF-8, or either side exceeding
/// [`MAX_SOURCE_BYTES`], yields no hunks at all — the file is reported as wholly
/// changed instead. Line ranges over bytes no extractor in this workspace can
/// address would be arithmetic dressed up as evidence.
fn hunks(base: &[u8], target: &[u8]) -> Vec<Hunk> {
    if base.len() > MAX_SOURCE_BYTES || target.len() > MAX_SOURCE_BYTES {
        return Vec::new();
    }
    let (Ok(base), Ok(target)) = (std::str::from_utf8(base), std::str::from_utf8(target)) else {
        return Vec::new();
    };

    let input = gix::diff::blob::InternedInput::new(base, target);
    let diff =
        gix::diff::blob::diff_with_slider_heuristics(gix::diff::blob::Algorithm::Histogram, &input);
    let mut out: Vec<Hunk> = diff
        .hunks()
        .map(|hunk| Hunk {
            old_start: side_start(hunk.before.start, hunk.before.end),
            old_lines: hunk.before.end - hunk.before.start,
            new_start: side_start(hunk.after.start, hunk.after.end),
            new_lines: hunk.after.end - hunk.after.start,
        })
        .collect();
    out.sort_by_key(|hunk| (hunk.new_start, hunk.old_start));
    out
}

/// One side's first line, 1-based, or the line an empty side follows.
///
/// `git diff -U0` prints `@@ -2,0 +3 @@` for a line inserted after line two:
/// the side with no lines names the line the change comes after, so `0` there
/// means "before the first line". Keeping that convention means a caller can
/// place a zero-width insertion without a second rule for it.
const fn side_start(start: u32, end: u32) -> u32 {
    if start == end {
        start
    } else {
        start + 1
    }
}

/// What the working tree currently holds for one path, read twice.
///
/// The two reads are the whole point. Acquisition is the observation; the
/// revalidation immediately after is the read, and it re-derives the identity
/// rather than comparing recorded metadata, so a same-size same-timestamp
/// overwrite cannot pass for the file that was observed. The doubled read is
/// accepted: the alternative is a hunk list computed from bytes that no longer
/// exist, attached to definitions from a file that does.
fn read_target(repo: &GitRepo, path: &RelPath) -> Result<TargetSide> {
    let acquired = match acquire_for_source(Some(repo), repo.workdir(), path)? {
        AcquireOutcome::Acquired(content) => content,
        AcquireOutcome::Refused(state) => {
            return Ok(match state {
                ContentPathState::Missing => TargetSide::Absent,
                ContentPathState::Symlink | ContentPathState::NotRegular => TargetSide::NotAFile,
                ContentPathState::TooLarge => TargetSide::Oversize,
                ContentPathState::Raced => TargetSide::Raced,
            })
        }
    };
    match revalidate_source(Some(repo), repo.workdir(), path, &acquired)? {
        Revalidation::Fresh => Ok(TargetSide::Present(acquired)),
        Revalidation::Refused(_) => Ok(TargetSide::Raced),
    }
}

/// The tree one commit points at.
fn tree_of(repo: &GitRepo, commit: Oid) -> Result<gix::Tree<'_>> {
    repo.gix_repo()
        .find_object(object_id(commit)?)
        .map_err(|error| Error::Content(format!("commit lookup failed: {error}")))?
        .peel_to_tree()
        .map_err(|error| Error::Content(format!("tree lookup failed: {error}")))
}

/// The blob one tree holds at `path`, or `None` when it holds no file there.
fn tree_blob(tree: &gix::Tree<'_>, path: &RelPath) -> Result<Option<Oid>> {
    let entry = tree
        .lookup_entry_by_path(path.as_str())
        .map_err(|error| Error::Content(format!("tree entry lookup failed: {error}")))?;
    match entry {
        Some(entry) if is_file(entry.mode()) => Ok(Some(Oid::from_raw(entry.oid().as_bytes())?)),
        Some(_) | None => Ok(None),
    }
}

/// The bytes of one blob, read by the oid that already names them.
///
/// Reading by oid is how every base side gets its content here: the identity
/// comes from the tree that recorded it and is never recomputed, which is what
/// leaves [`crate::oid::hash_blob`] the single hashing path in this crate.
fn blob_bytes(repo: &GitRepo, oid: Oid) -> Result<Vec<u8>> {
    let object = repo
        .gix_repo()
        .find_object(object_id(oid)?)
        .map_err(|error| Error::Content(format!("object lookup failed: {error}")))?;
    Ok(object.detach().data)
}

/// Whether one blob is bigger than the largest file this module will line-diff.
///
/// Asked of the object header, so an oversized blob is skipped without ever
/// being loaded.
fn over_diff_cap(repo: &GitRepo, oid: Oid) -> Result<bool> {
    let header = repo
        .gix_repo()
        .find_header(object_id(oid)?)
        .map_err(|error| Error::Content(format!("object header lookup failed: {error}")))?;
    Ok(header.size() > u64::try_from(MAX_SOURCE_BYTES).unwrap_or(u64::MAX))
}

/// Whether a tree entry names bytes a delta could be about.
///
/// Trees and submodule gitlinks arrive alongside the blobs beneath them, and
/// neither is a file an endpoint can hand to a parser: a directory's own entry
/// says nothing its blobs have not already said, and entering a nested
/// repository is that repository's business.
fn is_file(mode: EntryMode) -> bool {
    mode.is_blob_or_symlink()
}

/// The addressable path one tree location names, if it names a file at all.
fn file_path(location: &BStr, mode: EntryMode) -> Option<RelPath> {
    if is_file(mode) {
        rel(location)
    } else {
        None
    }
}

/// The addressable path one Git location names, if any.
///
/// Two kinds of location are dropped, for the reasons
/// [`GitRepo::observe_state`] drops them. A path [`RelPath`] rejects is not an
/// unknown change — it is a path outside the corpus that no discovery could ever
/// reach. And this tool's own state directory is not a repository change even
/// when the ignore rule stamped into it has been deleted. Both endpoints must
/// drop exactly the same paths, or a committed comparison would report changes a
/// working-tree comparison structurally cannot.
fn rel(location: &BStr) -> Option<RelPath> {
    let path = RelPath::new(location.to_str().ok()?).ok()?;
    if rr_core::workspace::is_private_path(path.as_str()) {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_side_names_the_line_the_change_follows() {
        assert_eq!(side_start(0, 0), 0, "an insertion before line one");
        assert_eq!(side_start(2, 2), 2, "an insertion after line two");
        assert_eq!(side_start(2, 5), 3, "a replacement of lines three to five");
    }

    #[test]
    fn identical_bytes_have_no_hunks() {
        assert!(hunks(b"fn one() {}\n", b"fn one() {}\n").is_empty());
    }

    #[test]
    fn bytes_that_are_not_utf8_have_no_hunks() {
        assert!(hunks(b"\xff\xfe one\n", b"\xff\xfe two\n").is_empty());
    }

    #[test]
    fn a_file_over_the_source_cap_has_no_hunks() {
        let big = vec![b'\n'; MAX_SOURCE_BYTES + 1];
        assert!(hunks(b"one\n", &big).is_empty());
        assert!(hunks(&big, b"one\n").is_empty());
    }
}

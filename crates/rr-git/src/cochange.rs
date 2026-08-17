//! Files that historically change together, bounded so history cannot dominate.
//!
//! Correlation, labelled `probable` everywhere it surfaces: it points at a
//! forgotten test, never at an entry in `affected`. Two files that always moved
//! together are evidence that a human kept them in step, and that is the whole
//! claim — spelling it as a resolved edge would send an agent to edit a file
//! because it *used to* travel with another one, which is a hunch wearing the
//! clothes of a fact.
//!
//! Every bound below exists because a repository's past is far larger than the
//! change being explained. A window of [`HISTORY_WINDOW`] commits, a
//! bulk-rewrite cut-off, a minimum number of shared commits and a Jaccard floor
//! together mean one enormous reformatting commit cannot make every file in the
//! repository look related to every other one — which is exactly what an
//! unbounded co-change count reports on any repository that has ever been
//! reindented.
//!
//! Nothing here reads a clock. Commits are taken in graph order from the target
//! endpoint, so a rewritten committer date changes neither which commits a run
//! reads nor what it concludes, and no timestamp reaches the result.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gix::bstr::{BStr, ByteSlice};
use gix::objs::tree::EntryMode;
use serde::{Deserialize, Serialize};

use rr_core::cancel::CancelToken;
use rr_core::path::RelPath;
use rr_core::ranking::MARGIN_SCALE;
use rr_core::snapshot::SNAPSHOT_MAGIC;
use rr_core::walk::{collected_lang, WalkCfg};

use crate::content::object_id;
use crate::diff::ChangeTarget;
use crate::oid::Oid;
use crate::repo::GitRepo;
use crate::{Error, Result};

/// Newest non-merge commits any run will read.
pub const HISTORY_WINDOW: usize = 50;
/// Above this, or above [`MAX_COMMIT_SHARE_PERCENT`] of eligible files, a
/// commit is a bulk rewrite rather than evidence, and is skipped.
pub const MAX_COMMIT_PATHS: usize = 1_000;
/// Share of the eligible corpus one commit may touch before it is a bulk
/// rewrite; see [`MAX_COMMIT_PATHS`].
pub const MAX_COMMIT_SHARE_PERCENT: u32 = 20;
/// Fewest shared commits before a pair may be reported at all.
pub const MIN_TOGETHER: u32 = 3;
/// Jaccard floor, in parts per million: 0.30.
pub const MIN_JACCARD_PPM: u32 = 300_000;
/// Bumped when any constant above changes, so a result cached under other
/// rules is rejected rather than believed.
pub const COCHANGE_CONFIG_VERSION: u32 = 1;

/// The cache file's name inside the machine-local state directory.
const CACHE_FILE: &str = "cochange.bin";
/// End of the envelope's magic, and the start of its version word.
const MAGIC_END: usize = SNAPSHOT_MAGIC.len();
/// End of the envelope's `u32` version word.
const VERSION_END: usize = MAGIC_END + 4;
/// End of the envelope's `u64` payload length.
const LENGTH_END: usize = VERSION_END + 8;
/// Width of the whole header: magic, version, length, BLAKE3 checksum.
const HEADER_LEN: usize = LENGTH_END + 32;

/// How often two files changed in the same commit, inside the window.
///
/// `a` is the seed — a file the change set names — and `b` is the candidate the
/// value is filed under, so the two counts are never interchangeable: a seed
/// touched fifty times and a candidate touched three tells a different story
/// from the reverse, and a single "commits" field could not tell either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CoChange {
    /// Commits inside the window that touched both files.
    pub together: u32,
    /// Commits inside the window that touched the seed.
    pub commits_a: u32,
    /// Commits inside the window that touched the candidate.
    pub commits_b: u32,
    /// `together / (commits_a + commits_b - together)`, in millionths.
    ///
    /// Serialized as a six-decimal string (`"0.301234"`) exactly as
    /// [`rr_core::ranking::Score`] is, because an `f64` in a golden file is a
    /// platform-dependent golden: the ratio is computed in integers, stored in
    /// integers and printed from integers, so no rounding mode ever enters the
    /// comparison.
    #[serde(serialize_with = "six_decimals")]
    pub jaccard_ppm: u32,
}

/// Writes a parts-per-million quantity as a six-decimal string.
///
/// `collect_str` rather than a float: the digits come from integer division and
/// remainder, so the rendered value is the stored value and cannot drift with a
/// platform's formatting of `f64`.
///
/// The reference is `serde`'s `serialize_with` signature, not a choice.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn six_decimals<S: serde::Serializer>(ppm: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&format_args!(
        "{}.{:06}",
        ppm / MARGIN_SCALE,
        ppm % MARGIN_SCALE
    ))
}

/// Files the seeds historically changed with, inside the bounded window.
///
/// `walk` decides what counts as a source file, through [`collected_lang`] — the
/// same answer discovery gives, so co-change can never point at a file `rr`
/// would not read. That set is also the denominator of
/// [`MAX_COMMIT_SHARE_PERCENT`] and part of the cache key, because two runs
/// configured for different languages see different corpora on one commit.
///
/// Seeds are excluded from their own answer: a seed is already in the change
/// set, and restating the input as a finding is not evidence. An empty seed
/// list, or an endpoint with no commit behind it, is an empty map rather than an
/// error — there is nothing to correlate, which is not a failure.
///
/// # Errors
/// Returns [`Error::Content`] when the repository cannot produce the target
/// tree, the history walk or a commit's own diff, [`Error::Cancelled`] when
/// cancellation was requested mid-walk, and [`Error::Oid`] for an object id the
/// repository refuses. A cache fault is none of these: it degrades co-change to
/// a recomputation and is never reported as a failure.
pub fn co_changed(
    repo: &GitRepo,
    target: &ChangeTarget,
    seeds: &[RelPath],
    walk: &WalkCfg,
    cancel: &CancelToken,
) -> Result<BTreeMap<RelPath, CoChange>> {
    if seeds.is_empty() {
        return Ok(BTreeMap::new());
    }
    let Some(tip) = tip_commit(target) else {
        return Ok(BTreeMap::new());
    };

    let eligible = eligible_paths(repo, tip, walk)?;
    let key = HistoryKey::of(tip, &eligible);
    let root = repo.workdir();
    let commits = if let Some(commits) = read_cache(root, &key, eligible.len()) {
        commits
    } else {
        let cached = CachedHistory {
            key,
            commits: window(repo, tip, &eligible, cancel)?,
        };
        write_cache(root, &cached);
        cached.commits
    };
    Ok(evidence(&commits, &eligible, seeds))
}

/// The commit whose history one endpoint names.
///
/// A working tree has no history of its own — uncommitted edits are not commits
/// — so co-change reads the history behind `HEAD`, and an unborn `HEAD` has
/// none. That also makes the two endpoint kinds interchangeable here, which is
/// why a working-tree run and a run naming the same commit share one cache.
const fn tip_commit(target: &ChangeTarget) -> Option<Oid> {
    match target {
        ChangeTarget::Tree { commit, .. } => Some(*commit),
        ChangeTarget::Worktree { head } => head.commit(),
    }
}

/// Every source file the target endpoint holds, sorted and unique.
///
/// Sorted because the list's digest is half the cache key and a digest over an
/// unordered list is a digest of the traversal, not of the corpus.
fn eligible_paths(repo: &GitRepo, tip: Oid, walk: &WalkCfg) -> Result<Vec<RelPath>> {
    let tree = commit_tree(repo, object_id(tip)?)?;
    let entries = tree
        .traverse()
        .breadthfirst
        .files()
        .map_err(|error| Error::Content(format!("tree traversal failed: {error}")))?;
    let mut paths: Vec<RelPath> = entries
        .iter()
        .filter(|entry| entry.mode.is_blob_or_symlink())
        .filter_map(|entry| eligible_path(entry.filepath.as_ref(), walk))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The addressable source path one tree location names, if it names one.
///
/// The two rejections are [`crate::diff`]'s, for its reasons: a path
/// [`RelPath`] refuses is outside the corpus no discovery could reach, and this
/// tool's own state directory is not repository history.
fn eligible_path(location: &BStr, walk: &WalkCfg) -> Option<RelPath> {
    let path = RelPath::new(location.to_str().ok()?).ok()?;
    if rr_core::workspace::is_private_path(path.as_str())
        || collected_lang(path.as_str(), walk).is_none()
    {
        return None;
    }
    Some(path)
}

/// The commits one run reads, as the eligible paths each of them touched.
///
/// Merges are skipped rather than diffed. A merge against its first parent
/// reports a whole branch as one edit, so every pair on that branch is counted
/// twice — once from the commits that made it and once from the merge that
/// carried it — and double counting is how a floor gets cleared without
/// evidence.
///
/// [`HISTORY_WINDOW`] counts commits *read*, including the bulk rewrites this
/// drops, because the window is what bounds a run's cost: a run that skipped a
/// thousand bulk commits before finding fifty ordinary ones would have read a
/// thousand and fifty.
fn window(
    repo: &GitRepo,
    tip: Oid,
    eligible: &[RelPath],
    cancel: &CancelToken,
) -> Result<Vec<Vec<u32>>> {
    let index = numbered(eligible);
    let history = repo
        .gix_repo()
        .rev_walk(Some(object_id(tip)?))
        .all()
        .map_err(|error| Error::Content(format!("history walk failed: {error}")))?;

    let mut read = 0usize;
    let mut commits = Vec::new();
    for info in history {
        if read == HISTORY_WINDOW {
            break;
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let info = info.map_err(|error| Error::Content(format!("history walk failed: {error}")))?;
        if info.parent_ids.len() > 1 {
            continue;
        }
        read += 1;
        let touched = touched_paths(repo, &index, info.id, info.parent_ids.first().copied())?;
        if is_bulk(touched.count, eligible.len()) {
            continue;
        }
        commits.push(touched.eligible);
    }
    Ok(commits)
}

/// The eligible paths numbered by their position in the sorted list.
///
/// Indices are `u32` so the cached payload stays small; a corpus with more than
/// four billion source files has no addressable position here, and a path that
/// cannot be numbered is simply never counted rather than counted as another.
fn numbered(eligible: &[RelPath]) -> BTreeMap<&str, u32> {
    eligible
        .iter()
        .enumerate()
        .filter_map(|(position, path)| Some((path.as_str(), u32::try_from(position).ok()?)))
        .collect()
}

/// What one commit contributed, before the bulk test decides whether to keep it.
struct Touched {
    /// Every file entry the commit changed, eligible or not.
    count: usize,
    /// Indices of the eligible paths among them, sorted and unique.
    eligible: Vec<u32>,
}

/// Which paths one commit changed, against its first parent.
///
/// A root commit is compared against no tree at all, which is how Git reports
/// it: every file it introduced counts, and on any real repository that first
/// commit is a bulk rewrite the next test drops.
///
/// Rename tracking is deliberately off. Co-change counts the paths a commit
/// touched, and a rename touched two of them; resolving it into one path would
/// hide half of the movement that co-change exists to notice, and paying for
/// similarity detection on fifty commits would spend the budget the window is
/// there to protect.
fn touched_paths(
    repo: &GitRepo,
    index: &BTreeMap<&str, u32>,
    commit: gix::ObjectId,
    parent: Option<gix::ObjectId>,
) -> Result<Touched> {
    use gix::object::tree::diff::ChangeDetached as Change;

    let tree = commit_tree(repo, commit)?;
    let parent_tree = parent.map(|id| commit_tree(repo, id)).transpose()?;
    let changes = repo
        .gix_repo()
        .diff_tree_to_tree(
            parent_tree.as_ref(),
            Some(&tree),
            gix::diff::Options::default(),
        )
        .map_err(|error| Error::Content(format!("tree comparison failed: {error}")))?;

    let mut touched = Touched {
        count: 0,
        eligible: Vec::new(),
    };
    for change in &changes {
        match change {
            Change::Addition {
                location,
                entry_mode,
                ..
            }
            | Change::Deletion {
                location,
                entry_mode,
                ..
            }
            | Change::Modification {
                location,
                entry_mode,
                ..
            } => record(location.as_ref(), *entry_mode, index, &mut touched),
            Change::Rewrite {
                location,
                entry_mode,
                source_location,
                source_entry_mode,
                ..
            } => {
                record(location.as_ref(), *entry_mode, index, &mut touched);
                record(
                    source_location.as_ref(),
                    *source_entry_mode,
                    index,
                    &mut touched,
                );
            }
        }
    }
    touched.eligible.sort_unstable();
    touched.eligible.dedup();
    Ok(touched)
}

/// Counts one changed entry, and files it under its index if it has one.
///
/// A path outside the eligible set still counts toward the bulk test: a commit
/// that rewrote five thousand generated files is a bulk rewrite whatever it
/// touched. It is never reported, because a file discovery would not read cannot
/// be the forgotten test this evidence exists to find.
fn record(location: &BStr, mode: EntryMode, index: &BTreeMap<&str, u32>, touched: &mut Touched) {
    if !mode.is_blob_or_symlink() {
        return;
    }
    touched.count += 1;
    if let Some(position) = location.to_str().ok().and_then(|path| index.get(path)) {
        touched.eligible.push(*position);
    }
}

/// Whether one commit is a bulk rewrite rather than evidence.
///
/// Both halves are needed. A thousand-path commit is a rewrite in any
/// repository; a hundred-path commit is a rewrite in a repository of two
/// hundred files and unremarkable in one of ten thousand. The share is compared
/// by multiplication, never by dividing into a percentage, so a rewrite cannot
/// round its way back under the bar.
fn is_bulk(touched: usize, eligible: usize) -> bool {
    let share = usize::try_from(MAX_COMMIT_SHARE_PERCENT).unwrap_or(usize::MAX);
    touched > MAX_COMMIT_PATHS || touched.saturating_mul(100) > eligible.saturating_mul(share)
}

/// The reportable pairs, keyed by the candidate each one points at.
///
/// Where two seeds reach one candidate the stronger evidence wins, ordered by
/// `(jaccard_ppm, together)` with the lowest-numbered seed keeping a tie, so the
/// answer cannot depend on the order the seeds were handed in.
fn evidence(
    commits: &[Vec<u32>],
    eligible: &[RelPath],
    seeds: &[RelPath],
) -> BTreeMap<RelPath, CoChange> {
    let index = numbered(eligible);
    let seed_ids: BTreeSet<u32> = seeds
        .iter()
        .filter_map(|seed| index.get(seed.as_str()).copied())
        .collect();
    if seed_ids.is_empty() {
        return BTreeMap::new();
    }

    let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
    let mut shared: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for commit in commits {
        for position in commit {
            *counts.entry(*position).or_default() += 1;
        }
        for seed in commit.iter().filter(|position| seed_ids.contains(position)) {
            for other in commit
                .iter()
                .filter(|position| !seed_ids.contains(position))
            {
                *shared.entry((*seed, *other)).or_default() += 1;
            }
        }
    }

    let mut found: BTreeMap<RelPath, CoChange> = BTreeMap::new();
    for ((seed, other), together) in shared {
        let Some(value) = reportable(together, count_of(&counts, seed), count_of(&counts, other))
        else {
            continue;
        };
        let Some(path) = path_at(eligible, other) else {
            continue;
        };
        if found
            .get(path)
            .is_none_or(|held| strength(&value) > strength(held))
        {
            found.insert(path.clone(), value);
        }
    }
    found
}

/// One pair's counts as a reportable value, or `None` when a bound rejects it.
///
/// The two bounds answer different objections. [`MIN_TOGETHER`] rejects a
/// coincidence: two files that changed together once changed together by
/// accident. [`MIN_JACCARD_PPM`] rejects a busy file: a file touched in every
/// commit shares commits with everything and correlates with nothing.
fn reportable(together: u32, commits_a: u32, commits_b: u32) -> Option<CoChange> {
    if together < MIN_TOGETHER {
        return None;
    }
    let union = commits_a.checked_add(commits_b)?.checked_sub(together)?;
    let jaccard_ppm = jaccard(together, union);
    (jaccard_ppm >= MIN_JACCARD_PPM).then_some(CoChange {
        together,
        commits_a,
        commits_b,
        jaccard_ppm,
    })
}

/// `together / union` in parts per million.
///
/// Truncating, deliberately: a ratio that would round up to the floor did not
/// reach it, and [`MIN_JACCARD_PPM`] is a floor rather than a target.
fn jaccard(together: u32, union: u32) -> u32 {
    if union == 0 {
        return 0;
    }
    let scaled = u64::from(together) * u64::from(MARGIN_SCALE) / u64::from(union);
    u32::try_from(scaled).unwrap_or(MARGIN_SCALE)
}

/// The total order two candidate values for one path are compared by.
const fn strength(value: &CoChange) -> (u32, u32) {
    (value.jaccard_ppm, value.together)
}

/// How many windowed commits touched one numbered path.
fn count_of(counts: &BTreeMap<u32, u32>, position: u32) -> u32 {
    counts.get(&position).copied().unwrap_or_default()
}

/// The path one index names, if it names one at all.
fn path_at(eligible: &[RelPath], position: u32) -> Option<&RelPath> {
    eligible.get(usize::try_from(position).ok()?)
}

/// The tree one commit points at.
fn commit_tree(repo: &GitRepo, commit: gix::ObjectId) -> Result<gix::Tree<'_>> {
    repo.gix_repo()
        .find_object(commit)
        .map_err(|error| Error::Content(format!("commit lookup failed: {error}")))?
        .peel_to_tree()
        .map_err(|error| Error::Content(format!("tree lookup failed: {error}")))
}

/// What one cached history is a history *of*.
///
/// Three of the four things that decide whether a file may be believed are
/// here: the commit the window was read from, the width of that window, and a
/// digest of the eligible-path list the stored indices point into. The fourth,
/// [`COCHANGE_CONFIG_VERSION`], is the envelope's version word, exactly as the
/// snapshot's schema version is — a file written under other rules is refused
/// before its payload is decoded at all.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryKey {
    /// The commit the window was read from.
    target: Oid,
    /// [`HISTORY_WINDOW`] as it stood when the file was written.
    window: u64,
    /// BLAKE3 of the sorted eligible-path list.
    paths: [u8; 32],
}

impl HistoryKey {
    /// The key one run needs its cache to match.
    ///
    /// Each path is followed by a `NUL` in the digest, because concatenation
    /// without a separator makes `["ab", "c"]` and `["a", "bc"]` the same bytes
    /// — two different corpora sharing one cache entry.
    fn of(target: Oid, eligible: &[RelPath]) -> Self {
        let mut hasher = blake3::Hasher::new();
        for path in eligible {
            hasher.update(path.as_str().as_bytes());
            hasher.update(&[0]);
        }
        Self {
            target,
            window: u64::try_from(HISTORY_WINDOW).unwrap_or(u64::MAX),
            paths: *hasher.finalize().as_bytes(),
        }
    }
}

/// The payload one cache file holds.
///
/// The history is cached, not the answer: the windowed commits depend on the
/// endpoint and the corpus and not on which files a change set happens to
/// touch, so every impact run on one commit reuses the one expensive part —
/// reading history — however different their seeds are.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CachedHistory {
    /// What this history is a history of; see [`HistoryKey`].
    key: HistoryKey,
    /// Per kept commit, the eligible paths it touched, as indices into the
    /// sorted eligible-path list [`HistoryKey::paths`] digests.
    commits: Vec<Vec<u32>>,
}

/// The cache file, in the directory [`rr_core::workspace::LOCAL_DIR`] reserves
/// for machine-local artifacts, beside the snapshot and the fact cache.
fn cache_path(root: &Path) -> PathBuf {
    rr_core::workspace::local_dir(root).join(CACHE_FILE)
}

/// The cached history, if the file on disk is exactly the one this run needs.
///
/// Every fault is the same `None` — absent, unreadable, truncated, trailing
/// bytes, a wrong checksum, another key, an index pointing past the
/// eligible-path list — because each one costs a recomputation and none of them
/// may cost a wrong answer. That is also why a cache fault never rises above a
/// warning where it is reported: nothing in this file can make co-change wrong,
/// only slow.
fn read_cache(root: &Path, key: &HistoryKey, eligible: usize) -> Option<Vec<Vec<u32>>> {
    let bytes = std::fs::read(cache_path(root)).ok()?;
    let payload = payload_of(&bytes)?;
    let (cached, rest) = postcard::take_from_bytes::<CachedHistory>(payload).ok()?;
    if !rest.is_empty() || &cached.key != key {
        return None;
    }
    let bound = u32::try_from(eligible).unwrap_or(u32::MAX);
    let addressable = cached
        .commits
        .iter()
        .flatten()
        .all(|position| *position < bound);
    addressable.then_some(cached.commits)
}

/// The payload inside one envelope, or `None` if this is not that envelope.
///
/// The layout is `crates/rr-core/src/snapshot.rs`'s, field for field and in its
/// order: the same magic, a version word, a payload length, a BLAKE3 checksum
/// over the payload. One length equality rejects both halves of the truncation
/// question at once — fewer bytes than the header claims, and more — and the
/// checksum is only consulted once the length agrees, because a digest over
/// bytes of unknown extent proves nothing.
///
/// Sharing the magic is safe in both directions: the version word carries
/// [`COCHANGE_CONFIG_VERSION`] where the snapshot's carries
/// [`rr_core::snapshot::SNAPSHOT_SCHEMA_VERSION`], so neither file can be
/// decoded as the other.
fn payload_of(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < HEADER_LEN || bytes[..MAGIC_END] != SNAPSHOT_MAGIC {
        return None;
    }
    let version = u32::from_le_bytes(bytes[MAGIC_END..VERSION_END].try_into().ok()?);
    if version != COCHANGE_CONFIG_VERSION {
        return None;
    }
    let length = usize::try_from(u64::from_le_bytes(
        bytes[VERSION_END..LENGTH_END].try_into().ok()?,
    ))
    .ok()?;
    if bytes.len() != HEADER_LEN.checked_add(length)? {
        return None;
    }
    let payload = &bytes[HEADER_LEN..];
    (blake3::hash(payload).as_bytes() == &bytes[LENGTH_END..HEADER_LEN]).then_some(payload)
}

/// One payload wrapped in the envelope [`payload_of`] reads back.
fn envelope(payload: &[u8]) -> Option<Vec<u8>> {
    let length = u64::try_from(payload.len()).ok()?;
    let mut bytes = Vec::with_capacity(HEADER_LEN.saturating_add(payload.len()));
    bytes.extend_from_slice(&SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&COCHANGE_CONFIG_VERSION.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(blake3::hash(payload).as_bytes());
    bytes.extend_from_slice(payload);
    Some(bytes)
}

/// Writes the history back, best effort.
///
/// Every failure is dropped on purpose: a run that could not leave a cache
/// behind has still answered the question it was asked. No temporary file and no
/// fsync either — the checksum is what makes a torn write self-detecting, and a
/// refused cache is a recomputation rather than a wrong answer.
fn write_cache(root: &Path, cached: &CachedHistory) {
    let Ok(payload) = postcard::to_allocvec(cached) else {
        return;
    };
    let Some(bytes) = envelope(&payload) else {
        return;
    };
    if rr_core::workspace::ensure_private(root).is_err() {
        return;
    }
    let path = cache_path(root);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = std::fs::write(&path, &bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> HistoryKey {
        HistoryKey::of(
            crate::oid::hash_blob(b"tip", crate::oid::HashAlgo::Sha1),
            &[RelPath::new("a.rs").unwrap_or_else(|_| unreachable!("a.rs is a relative path"))],
        )
    }

    #[test]
    fn a_jaccard_ratio_renders_as_six_decimals() {
        let value = CoChange {
            together: 3,
            commits_a: 3,
            commits_b: 10,
            jaccard_ppm: MIN_JACCARD_PPM,
        };
        let json = serde_json::to_string(&value).unwrap_or_default();
        assert!(
            json.contains("\"jaccard_ppm\":\"0.300000\""),
            "the ratio must be a six-decimal string, not a float: {json}"
        );
    }

    #[test]
    fn a_ratio_that_would_round_up_to_the_floor_is_below_it() {
        assert_eq!(jaccard(3, 10), MIN_JACCARD_PPM);
        assert!(jaccard(3, 11) < MIN_JACCARD_PPM);
        assert_eq!(jaccard(1, 0), 0);
    }

    #[test]
    fn bulk_is_an_absolute_and_a_relative_bound_at_once() {
        assert!(!is_bulk(MAX_COMMIT_PATHS, MAX_COMMIT_PATHS * 100));
        assert!(is_bulk(MAX_COMMIT_PATHS + 1, MAX_COMMIT_PATHS * 100));
        assert!(!is_bulk(2, 10));
        assert!(is_bulk(3, 10));
    }

    #[test]
    fn the_envelope_refuses_truncation_and_trailing_bytes() {
        let bytes = envelope(b"payload").unwrap_or_default();
        assert_eq!(payload_of(&bytes), Some(&b"payload"[..]));

        let mut extended = bytes.clone();
        extended.push(0);
        assert_eq!(payload_of(&extended), None, "trailing bytes are refused");

        let truncated = &bytes[..bytes.len() - 1];
        assert_eq!(payload_of(truncated), None, "a short payload is refused");

        let mut corrupt = bytes.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert_eq!(payload_of(&corrupt), None, "a wrong checksum is refused");

        let mut other_version = bytes;
        other_version[MAGIC_END] = other_version[MAGIC_END].wrapping_add(1);
        assert_eq!(
            payload_of(&other_version),
            None,
            "another config version is refused"
        );
    }

    #[test]
    fn a_cached_history_round_trips_through_its_payload() {
        let cached = CachedHistory {
            key: key(),
            commits: vec![vec![0, 1], vec![], vec![1]],
        };
        let payload = postcard::to_allocvec(&cached).unwrap_or_default();
        let bytes = envelope(&payload).unwrap_or_default();
        let read = payload_of(&bytes)
            .and_then(|payload| postcard::take_from_bytes::<CachedHistory>(payload).ok());
        assert_eq!(read.map(|(value, _)| value), Some(cached));
    }
}

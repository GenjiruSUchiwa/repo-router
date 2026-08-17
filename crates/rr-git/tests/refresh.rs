//! The differential oracle.
//!
//! There is exactly one thing an incremental refresh must never do: disagree
//! with the full rebuild it is pretending to be. Every test here drives the
//! repository through a real operation, refreshes incrementally, then rebuilds
//! from nothing and compares the published bytes. A difference of a single byte
//! is a wrong answer that a user would experience as a query returning stale
//! results, so nothing weaker than byte equality is asserted.
//!
//! The comparison is only meaningful if the incremental run actually took the
//! incremental path. A test that silently fell back to a full rebuild would
//! compare a full rebuild against a full rebuild and pass forever, so the mode
//! each operation is expected to reach is asserted alongside the bytes.

mod common;

use std::path::Path;

use common::{
    git, git_add_and_commit, init_git_repo, init_git_repo_with, mtime_of, porcelain,
    set_index_mtime, set_mtime, write,
};
use rr_core::cancel::CancelToken;
use rr_core::index::WorkerStats;
use rr_core::path::RelPath;
use rr_core::refresh::{RefreshMode, RefreshReport, ReportedMode};
use rr_core::snapshot::SnapshotStore;
use rr_core::walk::discover;
use rr_core::FactCache;
use rr_git::map::BuildContext;
use rr_git::plan::{observe, Published};

const THREADS: usize = 2;

fn refresh(dir: &Path, mode: RefreshMode) -> RefreshReport {
    rr_git::refresh(dir, THREADS, mode, &CancelToken::new()).expect("refresh failed")
}

/// The bytes currently published, which is the only thing readers ever see.
fn published(dir: &Path) -> Vec<u8> {
    SnapshotStore::new(dir)
        .read_published()
        .expect("reading the published snapshot failed")
        .expect("nothing was published")
}

/// Refreshes incrementally, then rebuilds in full, and insists they agree.
///
/// Returns the incremental report so a caller can assert what it cost.
fn agree(dir: &Path, step: &str) -> RefreshReport {
    let report = refresh(dir, RefreshMode::Incremental);
    let incremental = published(dir);

    let full_report = refresh(dir, RefreshMode::Full);
    let full = published(dir);

    assert_eq!(
        incremental.len(),
        full.len(),
        "after {step}: incremental and full snapshots differ in length"
    );
    assert!(
        incremental == full,
        "after {step}: incremental and full snapshots differ in content"
    );
    assert_eq!(
        full_report.mode,
        ReportedMode::Full,
        "after {step}: the control rebuild was not a full one"
    );
    report
}

fn seeded() -> tempfile::TempDir {
    let temp = init_git_repo();
    write(
        temp.path(),
        "src/lib.rs",
        "pub mod other;\npub fn one() -> u32 { other::two() }\n",
    );
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 2 }\n");
    write(temp.path(), "src/third.rs", "pub fn three() -> u32 { 3 }\n");
    git_add_and_commit(temp.path(), "seed");
    refresh(temp.path(), RefreshMode::Full);
    temp
}

#[test]
fn an_untouched_repository_agrees_and_does_no_work() {
    let temp = seeded();
    let report = agree(temp.path(), "no change");

    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.reparsed, 0, "nothing changed, so nothing was parsed");
    assert_eq!(
        report.content_reads, 0,
        "nothing changed, so nothing was read"
    );
    assert!(!report.snapshot_updated);
}

#[test]
fn an_untracked_file_agrees() {
    let temp = seeded();
    write(temp.path(), "src/fresh.rs", "pub fn fresh() -> u32 { 4 }\n");

    let report = agree(temp.path(), "an untracked file");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 1);
}

#[test]
fn an_unstaged_edit_agrees() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 22 }\n");

    let report = agree(temp.path(), "an unstaged edit");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 1);
}

#[test]
fn a_staged_edit_agrees() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 22 }\n");
    git(temp.path(), &["add", "src/other.rs"]);

    let report = agree(temp.path(), "a staged edit");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn a_deletion_agrees() {
    let temp = seeded();
    std::fs::remove_file(temp.path().join("src/third.rs")).expect("remove failed");

    let report = agree(temp.path(), "a deletion");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.removed, 1);
}

#[test]
fn a_rename_agrees() {
    let temp = seeded();
    git(temp.path(), &["mv", "src/third.rs", "src/renamed.rs"]);

    let report = agree(temp.path(), "a rename");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

/// The state that broke the first version of the plan validator: the old name
/// comes back as a different file while the rename is still pending.
#[test]
fn a_rename_whose_source_is_recreated_agrees() {
    let temp = seeded();
    git(temp.path(), &["mv", "src/third.rs", "src/renamed.rs"]);
    write(
        temp.path(),
        "src/third.rs",
        "pub fn replacement() -> u32 { 9 }\n",
    );

    let report = agree(temp.path(), "a recreated rename source");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn a_copy_agrees_and_leaves_its_source_alone() {
    let temp = seeded();
    std::fs::copy(
        temp.path().join("src/third.rs"),
        temp.path().join("src/fourth.rs"),
    )
    .expect("copy failed");

    let report = agree(temp.path(), "a copy");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn an_intent_to_add_file_agrees() {
    let temp = seeded();
    write(
        temp.path(),
        "src/pending.rs",
        "pub fn pending() -> u32 { 5 }\n",
    );
    git(temp.path(), &["add", "-N", "src/pending.rs"]);

    let report = agree(temp.path(), "an intent-to-add file");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn a_file_that_no_longer_parses_agrees() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two( -> { unclosed\n");

    let report = agree(temp.path(), "a file that no longer parses");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn a_nested_addition_agrees() {
    let temp = seeded();
    write(
        temp.path(),
        "src/deep/nested/inner.rs",
        "pub fn inner() -> u32 { 6 }\n",
    );

    let report = agree(temp.path(), "a nested addition");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

/// The sharpest form of the reverted-file hole: the reverted path is invisible
/// to the delta *and* the delta is non-empty, so the run takes the incremental
/// path and would happily retain the stale record if the previous dirty set
/// were not consulted.
#[test]
fn a_revert_hidden_behind_an_unrelated_edit_agrees() {
    let temp = seeded();
    let reverted = temp.path().join("src/other.rs");
    let original = std::fs::read(&reverted).expect("read failed");

    std::fs::write(&reverted, b"pub fn two() -> u32 { 99 }\n").expect("write failed");
    let dirtied = refresh(temp.path(), RefreshMode::Incremental);
    assert_eq!(dirtied.mode, ReportedMode::Incremental);
    std::fs::write(&reverted, &original).expect("restore failed");
    write(temp.path(), "src/unrelated.rs", "pub fn u() -> u32 { 8 }\n");

    let report = agree(temp.path(), "a revert hidden behind an unrelated edit");
    assert_eq!(
        report.mode,
        ReportedMode::Incremental,
        "the delta was usable; the reverted path just had to be reconsidered"
    );
}

#[test]
fn a_file_edited_back_to_its_previous_content_agrees() {
    let temp = seeded();
    let path = temp.path().join("src/other.rs");
    let original = std::fs::read(&path).expect("read failed");
    std::fs::write(&path, b"pub fn two() -> u32 { 99 }\n").expect("write failed");
    refresh(temp.path(), RefreshMode::Incremental);
    std::fs::write(&path, &original).expect("restore failed");

    let report = agree(temp.path(), "content restored to its previous value");
    assert_eq!(report.mode, ReportedMode::Incremental);
}

#[test]
fn a_new_commit_is_a_delta_and_agrees() {
    let temp = seeded();
    write(temp.path(), "src/fresh.rs", "pub fn fresh() -> u32 { 4 }\n");
    git_add_and_commit(temp.path(), "add a file");

    let report = agree(temp.path(), "a new commit");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 1);
    assert_eq!(report.reparsed, 1);
}

#[test]
fn a_commit_of_unindexed_files_republishes_without_parsing_anything() {
    let temp = seeded();
    write(temp.path(), "NOTES.md", "not collected\n");
    git_add_and_commit(temp.path(), "notes");

    let report = agree(temp.path(), "a commit of unindexed files");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 0);
    assert_eq!(report.reparsed, 0);
    assert!(report.snapshot_updated);
}

#[test]
fn a_committed_deletion_is_a_delta_and_agrees() {
    let temp = seeded();
    git(temp.path(), &["rm", "-q", "src/third.rs"]);
    git_add_and_commit(temp.path(), "remove a file");

    let report = agree(temp.path(), "a committed deletion");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.removed, 1);
}

#[test]
fn a_committed_rename_is_a_removal_and_an_addition_and_agrees() {
    let temp = seeded();
    git(temp.path(), &["mv", "src/third.rs", "src/renamed.rs"]);
    git_add_and_commit(temp.path(), "rename a file");

    let report = agree(temp.path(), "a committed rename");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.renamed, 0);
    assert_eq!(report.removed, 1);
}

#[test]
fn a_path_in_both_deltas_is_drafted_once_and_agrees() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 20 }\n");
    git_add_and_commit(temp.path(), "commit an edit");
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 21 }\n");

    let report = agree(temp.path(), "a path in both deltas");
    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.fallback_reason, None);
}

#[test]
fn a_committed_ignore_rule_falls_back_to_a_full_rebuild_and_agrees() {
    let temp = seeded();
    write(temp.path(), ".gitignore", "src/skipped.rs\n");
    git_add_and_commit(temp.path(), "ignore a path");

    let report = agree(temp.path(), "a committed ignore rule");
    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::DiscoveryRulesChanged)
    );
}

#[test]
fn an_edited_ignore_rule_falls_back_to_a_full_rebuild_and_agrees() {
    let temp = seeded();
    write(
        temp.path(),
        "src/skipped.rs",
        "pub fn skipped() -> u32 { 7 }\n",
    );
    refresh(temp.path(), RefreshMode::Incremental);

    write(temp.path(), ".gitignore", "src/skipped.rs\n");

    let report = agree(temp.path(), "a new ignore rule");
    assert_eq!(
        report.mode,
        ReportedMode::FallbackFull,
        "the rules that decide the corpus changed"
    );
}

#[test]
fn a_deleted_snapshot_rebuilds_and_agrees() {
    let temp = seeded();
    std::fs::remove_file(SnapshotStore::new(temp.path()).path()).expect("remove failed");

    let report = agree(temp.path(), "a deleted snapshot");
    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::MissingSnapshot)
    );
}

#[test]
fn a_corrupt_snapshot_rebuilds_and_agrees() {
    let temp = seeded();
    let path = SnapshotStore::new(temp.path()).path().to_path_buf();
    let mut bytes = std::fs::read(&path).expect("read failed");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).expect("write failed");

    let report = agree(temp.path(), "a corrupt snapshot");
    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::CorruptSnapshot)
    );
}

#[test]
fn a_repository_without_git_rebuilds_every_time_and_agrees() {
    let temp = tempfile::tempdir().expect("tempdir");
    write(temp.path(), "src/lib.rs", "pub fn one() -> u32 { 1 }\n");
    refresh(temp.path(), RefreshMode::Full);

    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 2 }\n");
    let report = agree(temp.path(), "a change outside Git");
    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::GitStatusUnavailable),
        "with no Git there is no delta to consult, only a rebuild"
    );
}

/// Each step is compared against a full rebuild, so a divergence is attributed
/// to the operation that caused it rather than to the end of a long sequence.
#[test]
fn a_long_sequence_of_operations_agrees_at_every_step() {
    let temp = seeded();
    let root = temp.path();

    write(root, "src/a.rs", "pub fn a() -> u32 { 1 }\n");
    agree(root, "step 1: add a.rs");

    write(root, "src/b.rs", "pub fn b() -> u32 { 2 }\n");
    git(root, &["add", "src/b.rs"]);
    agree(root, "step 2: stage b.rs");

    git_add_and_commit(root, "commit a and b");
    agree(root, "step 3: commit");

    write(root, "src/a.rs", "pub fn a() -> u32 { 11 }\n");
    agree(root, "step 4: edit a.rs");

    git(root, &["mv", "src/b.rs", "src/c.rs"]);
    agree(root, "step 5: rename b.rs to c.rs");

    std::fs::remove_file(root.join("src/third.rs")).expect("remove failed");
    agree(root, "step 6: delete third.rs");

    write(root, "src/b.rs", "pub fn b_again() -> u32 { 22 }\n");
    agree(root, "step 7: recreate b.rs");

    git_add_and_commit(root, "settle everything");
    agree(root, "step 8: commit again");
}

/// Refreshing twice with nothing in between must not rewrite the snapshot. A
/// tool that rewrites an identical file defeats every mtime-based cache built
/// on top of it, and makes "did anything change" unanswerable from outside.
#[test]
fn refreshing_twice_publishes_only_once() {
    let temp = seeded();
    write(temp.path(), "src/fresh.rs", "pub fn fresh() -> u32 { 4 }\n");

    let first = refresh(temp.path(), RefreshMode::Incremental);
    assert!(first.snapshot_updated, "the new file must be published");

    let second = refresh(temp.path(), RefreshMode::Incremental);
    assert!(
        !second.snapshot_updated,
        "nothing changed, so nothing should have been written"
    );
    assert_eq!(second.reparsed, 0);
}

/// A full rebuild of an unchanged repository must reach the same bytes and say
/// so, rather than republishing them.
#[test]
fn a_full_rebuild_of_an_unchanged_repository_publishes_nothing() {
    let temp = seeded();
    let report = refresh(temp.path(), RefreshMode::Full);

    assert_eq!(report.mode, ReportedMode::Full);
    assert!(
        !report.snapshot_updated,
        "the bytes were identical, so the file should not have been replaced"
    );
}

#[test]
fn a_second_refresh_is_refused_while_one_holds_the_claim() {
    let temp = seeded();
    write(temp.path(), "src/fresh.rs", "pub fn fresh() -> u32 { 4 }\n");

    let guard = rr_git::RepositoryWriteGuard::acquire(temp.path()).expect("first claim");
    let blocked = rr_git::refresh(
        temp.path(),
        THREADS,
        RefreshMode::Incremental,
        &CancelToken::new(),
    );

    assert!(
        matches!(blocked, Err(rr_git::Error::PublicationLocked { .. })),
        "expected a refusal, got {blocked:?}"
    );
    drop(guard);
}

/// A held claim must not block a run that has nothing to do. The no-op path
/// exists precisely so that the common case never contends for anything.
#[test]
fn a_no_op_refresh_ignores_a_held_claim() {
    let temp = seeded();
    let guard = rr_git::RepositoryWriteGuard::acquire(temp.path()).expect("first claim");

    let report = refresh(temp.path(), RefreshMode::Incremental);
    assert!(!report.snapshot_updated);
    drop(guard);
}

#[test]
fn cancellation_stops_the_refresh_without_publishing() {
    let temp = seeded();
    write(temp.path(), "src/fresh.rs", "pub fn fresh() -> u32 { 4 }\n");
    let before = published(temp.path());

    let cancel = CancelToken::new();
    cancel.cancel();
    let result = rr_git::refresh(temp.path(), THREADS, RefreshMode::Incremental, &cancel);

    assert!(
        matches!(result, Err(rr_git::Error::Cancelled)),
        "expected cancellation, got {result:?}"
    );
    assert_eq!(
        published(temp.path()),
        before,
        "a cancelled refresh must leave the previous snapshot in place"
    );
}

/// A file edited before the last refresh differs from `HEAD` for as long as it
/// stays uncommitted, so a delta that only consults `HEAD` reconsiders it on
/// every run for ever. On a working tree with uncommitted work — the normal
/// state of a repository an agent is being asked about — that is the fast path
/// never firing at all, which is the promise of the whole design.
#[test]
fn a_dirty_tree_that_stops_changing_reaches_the_no_op_path() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 22 }\n");

    let first = refresh(temp.path(), RefreshMode::Incremental);
    assert!(first.snapshot_updated, "the edit must be published");
    assert_eq!(first.reparsed, 1);

    let second = refresh(temp.path(), RefreshMode::Incremental);
    assert_eq!(
        second.changed, 0,
        "the file still differs from HEAD, but not from the snapshot"
    );
    assert_eq!(second.reparsed, 0, "nothing needed parsing a second time");
    assert!(!second.snapshot_updated);
}

/// Proving a dirty file unchanged means reading it, and a read no counter sees
/// is a cost that shows up in a benchmark as an unexplained regression. The
/// counters exist to tell "nothing changed" apart from "everything was re-read
/// and happened to agree", and planning must not open a hole in that.
#[test]
fn the_read_that_proves_a_dirty_file_unchanged_is_counted() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 22 }\n");
    refresh(temp.path(), RefreshMode::Incremental);

    let report = refresh(temp.path(), RefreshMode::Incremental);

    assert_eq!(report.reparsed, 0, "the no-op path must have been taken");
    assert_eq!(
        report.content_reads, 1,
        "the planner read one file and the report must say so"
    );
}

/// The same byte-for-byte oracle, over the states that make a delta lie.
///
/// Every step here is one where Git and discovery disagree about whether a path
/// matters — a file outside the corpus, a tracked file the walk skips, a name
/// that is a symlink one moment and a source file the next. Each is a place the
/// planner now declines to reconsider a path, and declining wrongly does not
/// produce a wrong label but a wrong snapshot, which only these bytes catch.
#[test]
fn a_sequence_of_states_the_delta_cannot_describe_agrees_at_every_step() {
    let temp = seeded();
    let root = temp.path();

    write(root, "README.md", "notes\n");
    git_add_and_commit(root, "add a file the index never collects");
    agree(root, "step 1: commit a non-source file");

    write(root, "README.md", "more notes\n");
    agree(root, "step 2: edit it, leaving it uncommitted");

    write(root, ".gitignore", "skipped/\n");
    agree(root, "step 3: add an uncommitted ignore rule");

    write(root, "skipped/hidden.rs", "pub fn hidden() -> u32 { 7 }\n");
    git(root, &["add", "-f", "skipped/hidden.rs"]);
    git_add_and_commit(root, "track a source file the walk ignores");
    agree(
        root,
        "step 4: commit a tracked file inside an ignored directory",
    );

    write(root, "skipped/hidden.rs", "pub fn hidden() -> u32 { 8 }\n");
    agree(root, "step 5: edit it");
    agree(root, "step 6: refresh again, which is where it settles");

    std::os::unix::fs::symlink("lib.rs", root.join("src/alias.rs")).expect("symlink failed");
    git_add_and_commit(root, "add a symlink named like a source file");
    agree(root, "step 7: commit a symlink");

    std::fs::remove_file(root.join("src/alias.rs")).expect("remove failed");
    write(root, "src/alias.rs", "pub fn alias() -> u32 { 9 }\n");
    agree(root, "step 8: replace the symlink with a real source file");

    git_add_and_commit(root, "settle everything");
    agree(root, "step 9: commit");
}

/// A directory that did not exist before must not hide the files in it.
///
/// Git can report untracked content either as the files themselves or as the
/// topmost new directory, and the planner now rules a path out by its name — a
/// directory has no extension, so a collapsed report would be declined and the
/// source files inside it would never be reconsidered. The observation asks for
/// files individually, and this is the test that says so out loud: it fails the
/// moment that setting changes, rather than at the point someone notices their
/// new module is missing from every query.
#[test]
fn a_brand_new_directory_is_reported_as_the_files_inside_it() {
    let temp = seeded();
    let root = temp.path();

    write(root, "fresh/module/deep.rs", "pub fn deep() -> u32 { 5 }\n");

    let report = agree(root, "a source file in a directory that did not exist");

    assert_eq!(
        report.changed, 1,
        "the delta named {} paths, so the new file was not reported individually",
        report.changed
    );
    assert_eq!(report.reparsed, 1, "the new file was not parsed");
}

/// The one repository change Git cannot see, and the proof that the fast path
/// does not make it worse.
///
/// Every field the index records is restored here: same length, same
/// modification time, and change time excluded by configuration — a setting
/// that exists precisely because some filesystems report a change time nobody
/// can trust. The index is aged forward so the racy-clean rule has nothing to
/// object to either. What is left is a file whose content changed and whose
/// stat says otherwise, and `git status` reports a clean tree.
///
/// The refresh reports no change, which is the only answer available to
/// anything that asks Git what happened. The claim being made is the narrower
/// one that can actually be kept: the incremental path introduces no staleness
/// of its own. A full rebuild reads the same stat, reaches the same
/// certification, and writes the same bytes — so the delta is not hiding
/// anything the rebuild would have found.
///
/// Worth stating plainly because it is the sharp edge of a Git-gated design:
/// `--full` is not a remedy for this one. Nothing short of making Git itself
/// see the file again is.
#[test]
fn a_rewrite_git_cannot_see_is_invisible_to_a_full_rebuild_too() {
    let temp = seeded();
    git(temp.path(), &["config", "core.trustctime", "false"]);

    let file = temp.path().join("src/other.rs");
    let (secs, nanos) = mtime_of(&file);
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 7 }\n");
    set_mtime(&file, secs, nanos);
    set_index_mtime(temp.path(), 2_000_000_000, 0);

    assert_eq!(
        porcelain(temp.path()),
        "",
        "the premise is that Git reports nothing; without it this proves nothing"
    );

    let report = agree(temp.path(), "a rewrite Git cannot see");

    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(
        report.changed, 0,
        "there is nothing for the plan to act on, and inventing one would be a guess"
    );
}

/// The stated budget for the common case, held to exactly.
///
/// One file changed in a clean repository should cost one read to extract it
/// and one to confirm the extraction still describes what is on disk. Not three,
/// which would mean planning re-read a file whose record could not have matched
/// anyway; and not one, which would mean the snapshot was published without
/// anybody checking that the bytes it names are still there.
///
/// An equality rather than a bound, because both directions are failures and a
/// bound only catches one of them.
#[test]
fn one_modified_file_costs_one_read_to_extract_and_one_to_confirm() {
    let temp = seeded();
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 22 }\n");

    let report = refresh(temp.path(), RefreshMode::Incremental);

    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 1);
    assert_eq!(
        report.content_reads, 2,
        "one read to build the record, one to stand behind it"
    );
}

/// An uncommitted `.gitattributes` changes what every tracked file *means*
/// without changing one byte of any of them.
///
/// Declaring `text eol=crlf` re-points the clean filter, so the canonical
/// content of every `.rs` file in the repository moves while Git reports a
/// single modified path. A delta taken at face value would recheck that one
/// path and retain every other record against content that now resolves
/// differently.
///
/// The rule digest is what prevents it, and this is the case that proves the
/// digest earns its cost: a committed change of the same file would have moved
/// `HEAD` and forced a rebuild for free.
#[test]
fn an_uncommitted_attributes_change_rebuilds_and_agrees() {
    let temp = seeded();
    write(temp.path(), ".gitattributes", "*.rs text eol=crlf\n");

    let report = agree(temp.path(), "an uncommitted attributes change");

    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::DiscoveryRulesChanged)
    );
}

/// The same repository under a different hash function.
///
/// A SHA-256 repository stores thirty-two-byte object names, and every place
/// this code compares an OID to one Git produced has to be sized by the
/// repository rather than by the common case. Nothing here is specific to the
/// operation being a modification — the point is that the whole cycle runs, the
/// delta is believed, and the bytes agree with a rebuild that hashed the same
/// way.
#[test]
fn a_sha256_repository_refreshes_and_agrees() {
    let temp = init_git_repo_with(&["--object-format=sha256"]);
    write(temp.path(), "src/lib.rs", "pub fn one() -> u32 { 1 }\n");
    git_add_and_commit(temp.path(), "seed");
    refresh(temp.path(), RefreshMode::Full);

    write(temp.path(), "src/lib.rs", "pub fn one() -> u32 { 11 }\n");

    let report = agree(temp.path(), "a modification in a sha256 repository");

    assert_eq!(report.mode, ReportedMode::Incremental);
    assert_eq!(report.changed, 1);
}

/// The very first thing a new repository is: initialised, with files in it, and
/// no commit yet.
///
/// Everything is untracked, which means everything is in the delta and stays
/// there — `git status` will keep reporting these files until somebody commits
/// them. A plan that took that at face value would recheck the whole repository
/// on every run and never once reach the no-op path, which is the same
/// never-settles failure as an uncommitted deletion wearing different clothes.
///
/// It settles because the snapshot records which paths it built from a dirty
/// tree, so the second run can ask whether the record still matches instead of
/// asking whether the file is committed.
#[test]
fn a_repository_with_no_commits_settles_and_agrees() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() -> u32 { 1 }\n");
    write(temp.path(), "src/other.rs", "pub fn two() -> u32 { 2 }\n");
    refresh(temp.path(), RefreshMode::Full);

    let report = agree(temp.path(), "an uncommitted repository");
    assert_eq!(report.mode, ReportedMode::Incremental);

    let settled = refresh(temp.path(), RefreshMode::Incremental);
    assert!(
        !settled.snapshot_updated,
        "an uncommitted repository that stopped changing has to stop reporting work"
    );
    assert_eq!(settled.reparsed, 0, "the no-op path must have been taken");
}

/// The commit that follows, which moves `HEAD` off nothing at all.
///
/// `HEAD` going from unborn to a commit is a change of kind rather than of
/// value, and comparing `Option<Oid>` makes it look like any other move. It has
/// to force a rebuild for the ordinary reason — every path's relationship to
/// `HEAD` just changed — and the bytes have to come out the same either way.
#[test]
fn the_first_commit_rebuilds_and_agrees() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() -> u32 { 1 }\n");
    refresh(temp.path(), RefreshMode::Full);

    git_add_and_commit(temp.path(), "first");

    let report = agree(temp.path(), "the first commit");
    assert_eq!(report.mode, ReportedMode::FallbackFull);
    assert_eq!(
        report.fallback_reason,
        Some(rr_core::refresh::FullReason::HeadChanged)
    );
}

/// A file that goes missing while it is being read, and is back before the
/// snapshot is assembled.
///
/// The build produces no record for it — there was nothing to read — and by
/// assembly time it is once again a perfectly ordinary dirty source file with
/// no record. That is indistinguishable, from the outside, from a file
/// discovery looked at and declined, and recording it as declined settles the
/// question the wrong way *for good*: the planner drops a declined path from
/// every future delta, `status` answers `fresh`, the no-op path is taken, and
/// no refresh ever looks at that file again.
///
/// The window cannot be scheduled by racing a real deletion against a real
/// build. It does not have to be: `run` takes the per-file decision from its
/// caller — that is how retention is injected — so a worker that reports one
/// file as vanished is the ordinary API used ordinarily, not a hook cut for a
/// test.
///
/// The second half is the one that matters. Publishing that snapshot and
/// letting an ordinary refresh loose on it is what turns "the list is right"
/// into "the file comes back", which is the only form of this the user ever
/// experiences.
#[test]
fn a_path_that_vanished_mid_build_is_not_recorded_as_one_discovery_declined() {
    let temp = seeded();
    write(temp.path(), "src/ghost.rs", "pub fn ghost() -> u32 { 9 }\n");
    let ghost = RelPath::try_from("src/ghost.rs").expect("bad path");

    let context = BuildContext::open(temp.path(), THREADS).expect("open failed");
    let files = discover(&context.work_root, &context.walk).expect("discovery failed");
    let cache = FactCache::open(&context.work_root).expect("cache failed");
    let observed = observe(&context, &CancelToken::new()).expect("observation failed");

    assert!(
        files.iter().any(|file| file.path == ghost),
        "discovery has to offer it, or there is no vanishing to speak of"
    );
    assert!(
        observed
            .as_ref()
            .is_some_and(|state| state.changes.iter().any(|change| change.path == ghost)),
        "it has to be dirty, or it never reaches the skipped list at all"
    );

    let built = context
        .run(&files, |worker, source| {
            if source.path == ghost {
                return Ok((None, WorkerStats::default()));
            }
            worker.process(source, &cache)
        })
        .expect("build failed");
    let outcome = context
        .assemble(built, observed.as_ref())
        .expect("assembly failed");

    assert!(
        outcome.snapshot.file_by_path(ghost.as_str()).is_none(),
        "the premise is that this build produced no record for it"
    );
    assert!(
        temp.path().join("src/ghost.rs").is_file(),
        "and that it is an ordinary file again by the time the list is written"
    );
    assert!(
        !outcome.snapshot.meta.skipped_paths.contains(&ghost),
        "a path discovery offered is no evidence about what discovery declines"
    );

    let store = SnapshotStore::new(temp.path());
    let envelope = store.encode(&outcome.snapshot).expect("encoding failed");
    store.publish(&envelope).expect("publication failed");

    let report = refresh(temp.path(), RefreshMode::Incremental);
    assert_eq!(report.mode, ReportedMode::Incremental);

    let published = Published::load(&store).expect("loading failed");
    let snapshot = published.snapshot().expect("nothing was published");
    assert!(
        snapshot.file_by_path(ghost.as_str()).is_some(),
        "the next refresh has to reconsider it; otherwise the file is gone from \
         the index for as long as nobody touches it again"
    );
}

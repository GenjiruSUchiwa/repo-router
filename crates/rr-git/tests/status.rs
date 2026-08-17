//! `rr status` describes what `rr refresh` would do — and must not do it.
//!
//! Two properties are worth more than any individual label. The first is
//! agreement: whenever status says `fresh`, a refresh must report `unchanged`
//! and publish nothing, and whenever it says `stale`, a refresh must publish.
//! Asserting the pair rather than the label is what keeps the two commands from
//! drifting apart. The second is that status is read-only, which a repository
//! notices even when a report does not.

mod common;

use std::fs;
use std::path::Path;

use common::{git, git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::parser::Registry;
use rr_core::path::RelPath;
use rr_core::refresh::{FullReason, GitLabel, RefreshMode, SnapshotLabel, StatusReport};
use rr_core::snapshot::SnapshotStore;
use rr_core::walk::WalkCfg;
use rr_git::plan::{plan_for, Published};
use rr_git::rules::discovery_digest;
use rr_git::{refresh, status, ChangeKind, GitRepo, WorktreeChange};

fn seed(dir: &Path) {
    write(dir, "src/lib.rs", "pub fn one() {}\n");
    git_add_and_commit(dir, "seed");
}

fn published(dir: &Path) {
    refresh(dir, 1, RefreshMode::Full, &CancelToken::new()).expect("initial refresh failed");
}

fn look(dir: &Path) -> StatusReport {
    status(dir, &CancelToken::new()).expect("status failed")
}

/// Asserts the two properties that make a status worth printing.
///
/// *Honesty*: `fresh` must mean a refresh does nothing at all — not "publishes
/// nothing after re-reading the repository", which would make the label a
/// prediction rather than a fact.
///
/// *Convergence*: whatever the label, running a refresh must make it `fresh`.
/// A status that survives the refresh meant to clear it is worse than useless:
/// it tells a user to act, and acting does not change the answer. The dirty
/// working tree is where that fails, because a file edited before the last
/// refresh differs from `HEAD` forever.
fn agrees_with_refresh(dir: &Path) -> StatusReport {
    let report = look(dir);
    let refreshed = refresh(dir, 1, RefreshMode::Incremental, &CancelToken::new())
        .expect("refresh after status failed");

    if report.snapshot == SnapshotLabel::Fresh {
        assert!(
            !refreshed.snapshot_updated,
            "status said fresh but refresh republished the snapshot"
        );
        assert_eq!(
            refreshed.reparsed, 0,
            "status said fresh but refresh still parsed files"
        );
    }

    assert_eq!(
        look(dir).snapshot,
        SnapshotLabel::Fresh,
        "a refresh did not clear the {:?} it was told to clear",
        report.snapshot
    );
    report
}

#[test]
fn a_repository_matching_its_snapshot_is_fresh_and_clean() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Clean);
    assert_eq!(report.snapshot, SnapshotLabel::Fresh);
    assert_eq!(report.stale_paths, Some(0));
    assert!(report.head.is_some());
}

#[test]
fn status_before_any_refresh_reports_a_missing_snapshot() {
    let temp = init_git_repo();
    seed(temp.path());

    let report = look(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Missing);
    assert_eq!(report.git, GitLabel::Clean);

    assert_eq!(report.stale_paths, None);
    assert_eq!(report.unresolved, 0);
}

#[test]
fn an_edited_file_makes_the_snapshot_stale_and_the_tree_dirty() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());
    write(
        temp.path(),
        "src/lib.rs",
        "pub fn one() {}\npub fn two() {}\n",
    );

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Dirty);
    assert_eq!(report.snapshot, SnapshotLabel::Stale);
    assert_eq!(report.stale_paths, Some(1));
}

#[test]
fn a_dirty_tree_that_was_indexed_dirty_is_still_fresh() {
    let temp = init_git_repo();
    seed(temp.path());
    write(temp.path(), "src/lib.rs", "pub fn edited() {}\n");
    published(temp.path());

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Dirty);
    assert_eq!(report.snapshot, SnapshotLabel::Fresh);
}

#[test]
fn a_reverted_file_is_reported_stale_even_though_git_says_nothing() {
    let temp = init_git_repo();
    seed(temp.path());
    write(temp.path(), "src/lib.rs", "pub fn edited() {}\n");
    published(temp.path());

    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Clean);
    assert_eq!(report.snapshot, SnapshotLabel::Stale);
    assert_eq!(report.stale_paths, Some(1));
}

#[test]
fn a_moved_head_is_stale_with_the_paths_the_commit_touched() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());
    write(temp.path(), "src/other.rs", "pub fn other() {}\n");
    git_add_and_commit(temp.path(), "second");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Clean);
    assert_eq!(report.snapshot, SnapshotLabel::Stale);
    assert_eq!(report.stale_paths, Some(1));
}

#[test]
fn a_changed_rule_is_stale_with_an_uncountable_number_of_paths() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());
    write(temp.path(), ".gitignore", "src/skipped.rs\n");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Stale);
    assert_eq!(report.stale_paths, None);
}

#[test]
fn a_conflicted_path_outranks_the_modifications_that_accompany_it() {
    let temp = init_git_repo();
    seed(temp.path());
    git(temp.path(), &["checkout", "-q", "-b", "side"]);
    write(temp.path(), "src/lib.rs", "pub fn side() {}\n");
    git_add_and_commit(temp.path(), "side edit");
    git(temp.path(), &["checkout", "-q", "-"]);
    write(temp.path(), "src/lib.rs", "pub fn main_branch() {}\n");
    git_add_and_commit(temp.path(), "main edit");

    let merge = std::process::Command::new("git")
        .args(["merge", "side"])
        .current_dir(temp.path())
        .output()
        .expect("git merge failed to run");
    assert!(
        !merge.status.success(),
        "the fixture must actually conflict"
    );

    let report = look(temp.path());

    assert_eq!(report.git, GitLabel::Conflicted);
}

#[test]
fn a_root_outside_git_can_be_described_but_not_compared() {
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    published(temp.path());

    let report = look(temp.path());

    assert_eq!(report.git, GitLabel::NoGit);
    assert_eq!(report.head, None);

    assert_eq!(report.snapshot, SnapshotLabel::Unknown);
    assert_eq!(report.stale_paths, None);
}

/// Every uncommitted change is permanent as far as Git status is concerned, so
/// every kind of it is a candidate for the same trap: reported for ever, and so
/// reconsidered for ever. These cover the arms an edit does not reach.
#[test]
fn an_uncommitted_deletion_stops_being_stale_once_it_is_absorbed() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    write(temp.path(), "src/other.rs", "pub fn two() {}\n");
    git_add_and_commit(temp.path(), "seed");
    published(temp.path());
    fs::remove_file(temp.path().join("src/other.rs")).expect("failed to delete");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Stale);
    assert_eq!(report.stale_paths, Some(1));
}

#[test]
fn an_uncommitted_rename_stops_being_stale_once_it_is_absorbed() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    write(temp.path(), "src/other.rs", "pub fn two() {}\n");
    git_add_and_commit(temp.path(), "seed");
    published(temp.path());
    git(temp.path(), &["mv", "src/other.rs", "src/moved.rs"]);

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Stale);
}

#[test]
fn an_uncommitted_ignore_rule_stops_forcing_a_rebuild_once_it_is_absorbed() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());
    write(temp.path(), ".gitignore", "generated/\n");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Stale);
}

#[test]
fn a_damaged_snapshot_is_corrupt_rather_than_stale() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());

    let path = temp.path().join(".rr/local/snapshot.bin");
    let mut bytes = fs::read(&path).expect("published snapshot is unreadable");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&path, &bytes).expect("failed to damage the snapshot");

    let report = look(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Corrupt);
    assert_eq!(report.stale_paths, None);
    assert_eq!(report.unresolved, 0);
}

#[test]
fn status_writes_nothing_and_takes_no_lock() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());

    let before = tree_fingerprint(temp.path());
    let _ = look(temp.path());
    let after = tree_fingerprint(temp.path());

    assert_eq!(before, after, "status modified the repository");
}

#[test]
fn status_creates_no_state_directory_in_a_repository_it_has_never_built() {
    let temp = init_git_repo();
    seed(temp.path());

    let _ = look(temp.path());

    assert!(
        !temp.path().join(".rr").exists(),
        "status created the state directory"
    );
}

#[test]
fn a_cancelled_status_fails_rather_than_reporting_a_clean_tree() {
    let temp = init_git_repo();
    seed(temp.path());
    published(temp.path());

    let cancel = CancelToken::new();
    cancel.cancel();

    let result = status(temp.path(), &cancel);

    assert!(
        matches!(result, Err(rr_git::Error::Cancelled)),
        "a cancelled status must not answer, got {result:?}"
    );
}

#[test]
fn the_unresolved_count_comes_from_the_published_snapshot() {
    let temp = init_git_repo();
    write(
        temp.path(),
        "src/lib.rs",
        "pub fn one() { nowhere::missing(); }\n",
    );
    git_add_and_commit(temp.path(), "seed");
    published(temp.path());

    let report = look(temp.path());

    assert_eq!(report.snapshot, SnapshotLabel::Fresh);
    assert!(
        report.unresolved > 0,
        "a call into an unknown module should not resolve"
    );
}

/// Every path under the root with its bytes, so any creation, deletion or edit
/// anywhere shows up as a difference.
fn tree_fingerprint(root: &Path) -> Vec<(String, u64)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, meta.len()));
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// A file this index would never collect must not hold the snapshot hostage.
///
/// Git reports every tracked file it sees change, and almost none of them are
/// this index's business. If those reached the delta, any repository with an
/// uncommitted `README` edit would answer `stale` for as long as the edit
/// lived, and the refresh it demanded would find nothing to do.
#[test]
fn an_edit_to_a_file_the_index_never_collects_leaves_it_fresh() {
    let temp = init_git_repo();
    seed(temp.path());
    write(temp.path(), "README.md", "before\n");
    git_add_and_commit(temp.path(), "add a readme");
    published(temp.path());

    write(temp.path(), "README.md", "after\n");

    let report = agrees_with_refresh(temp.path());

    assert_eq!(report.git, GitLabel::Dirty, "git should still see the edit");
    assert_eq!(report.snapshot, SnapshotLabel::Fresh);
    assert_eq!(report.stale_paths, Some(0));
}

/// Ignore rules do not apply to tracked files, but the walk still skips them.
///
/// A source file committed inside an ignored directory is the one case where
/// the two disagree permanently: Git reports every edit to it, and discovery
/// never produces it. Only the previous build can settle which is right, since
/// it walked the tree with the file in it and wrote no record.
#[test]
fn an_edit_to_a_tracked_file_inside_an_ignored_directory_settles() {
    let temp = init_git_repo();
    seed(temp.path());
    write(temp.path(), ".gitignore", "skipped/\n");
    write(temp.path(), "skipped/hidden.rs", "pub fn hidden() {}\n");
    git(temp.path(), &["add", "-f", "skipped/hidden.rs"]);
    git_add_and_commit(temp.path(), "track a file the walk ignores");
    published(temp.path());

    write(
        temp.path(),
        "skipped/hidden.rs",
        "pub fn hidden(x: u8) {}\n",
    );

    let first = agrees_with_refresh(temp.path());
    let settled = agrees_with_refresh(temp.path());

    assert_eq!(first.snapshot, SnapshotLabel::Stale);
    assert_eq!(settled.snapshot, SnapshotLabel::Fresh);
    assert_eq!(settled.stale_paths, Some(0));
}

/// A conflict outside the corpus is a fact about Git, not about the index.
///
/// Both labels are reported, and they disagree on purpose: the tree really is
/// mid-merge, and the snapshot really is still true. Folding the conflict into
/// the snapshot label would mark the index untrustworthy — and `rr refresh`
/// would exit non-zero — for the whole of a merge that never touched a file it
/// contains.
#[test]
fn a_conflict_outside_the_corpus_leaves_the_snapshot_trustworthy() {
    let temp = init_git_repo();
    seed(temp.path());
    write(temp.path(), "notes.md", "base\n");
    git_add_and_commit(temp.path(), "base notes");
    git(temp.path(), &["checkout", "-q", "-b", "side"]);
    write(temp.path(), "notes.md", "theirs\n");
    git_add_and_commit(temp.path(), "their notes");
    git(temp.path(), &["checkout", "-q", "-"]);
    write(temp.path(), "notes.md", "ours\n");
    git_add_and_commit(temp.path(), "our notes");
    published(temp.path());

    let merge = std::process::Command::new("git")
        .args(["merge", "side"])
        .current_dir(temp.path())
        .output()
        .expect("git merge failed to run");
    assert!(
        !merge.status.success(),
        "the fixture must actually conflict"
    );

    let report = look(temp.path());

    assert_eq!(report.git, GitLabel::Conflicted);
    assert_eq!(report.snapshot, SnapshotLabel::Fresh);
}

/// Evidence that discovery declined a path must not outlive what it was about.
///
/// A skipped path is skipped for one of two reasons — a rule excluded it, or
/// what is there is not a file — and only the first is something a later run
/// can notice. A symlink that becomes a real source file is the case where
/// remembering "declined" would drop a file out of the index for good.
#[test]
fn a_symlink_that_becomes_a_source_file_is_indexed() {
    let temp = init_git_repo();
    seed(temp.path());
    std::os::unix::fs::symlink("lib.rs", temp.path().join("src/alias.rs"))
        .expect("failed to create the symlink");
    git_add_and_commit(temp.path(), "add a symlink named like a source file");
    published(temp.path());

    fs::remove_file(temp.path().join("src/alias.rs")).expect("failed to drop the symlink");
    std::os::unix::fs::symlink("../src/lib.rs", temp.path().join("src/alias.rs"))
        .expect("failed to repoint the symlink");
    agrees_with_refresh(temp.path());

    fs::remove_file(temp.path().join("src/alias.rs")).expect("failed to drop the symlink");
    write(temp.path(), "src/alias.rs", "pub fn alias() {}\n");
    agrees_with_refresh(temp.path());

    let report = refresh(temp.path(), 1, RefreshMode::Full, &CancelToken::new())
        .expect("full refresh failed");
    assert!(
        !report.snapshot_updated,
        "the incremental snapshot disagrees with a full build of the same tree"
    );
}

/// A delta that contradicts itself says so, instead of blaming Git.
///
/// `PlanDraft::build` rejects one path renamed to two targets, and the planner
/// used to report that rejection as `git-status-unavailable` — a diagnosis that
/// sends whoever is holding the tool to look at Git, which is the one place the
/// problem is not. The status was observed, and observed perfectly; two of its
/// items simply cannot both be true.
///
/// The observation is built by hand because Git will not produce this. That is
/// the point: the branch exists for the case where Git, or the version of it
/// vendored here, reports something this code cannot make sense of, and a
/// branch reachable only through a bug still has to say something honest when
/// it is reached.
#[test]
fn a_delta_that_contradicts_itself_is_not_reported_as_an_unreadable_repository() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    git_add_and_commit(temp.path(), "seed");
    refresh(
        temp.path(),
        1,
        RefreshMode::Full,
        &rr_core::cancel::CancelToken::new(),
    )
    .expect("seed refresh failed");

    let repo = GitRepo::discover(temp.path())
        .expect("discovery failed")
        .expect("fixture is not a git repository");
    let mut observed = repo
        .observe_state(&CancelToken::new())
        .expect("observation failed");

    let moved = |target: &str| WorktreeChange {
        kind: ChangeKind::Renamed,
        path: RelPath::try_from(target).expect("bad path"),
        source: Some(RelPath::try_from("src/lib.rs").expect("bad path")),
    };
    observed.changes = vec![moved("src/here.rs"), moved("src/there.rs")];

    let store = SnapshotStore::new(temp.path());
    let published = Published::load(&store).expect("loading the snapshot failed");
    let walk = WalkCfg {
        languages: Some(Registry::indexable()),
        threads: Some(1),
        ..WalkCfg::default()
    };
    let digest = discovery_digest(Some(&repo), &walk, Some(&observed));

    let planned = plan_for(
        RefreshMode::Incremental,
        &published,
        Some(&observed),
        digest,
        Some(&repo),
        &walk,
    );

    assert_eq!(
        planned.plan.reason(),
        Some(FullReason::ContradictoryDelta),
        "the delta contradicted itself; Git did not fail to answer"
    );
}

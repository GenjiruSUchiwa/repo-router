//! The observed-state matrix.
//!
//! Every row here is a real repository state that a refresh must classify
//! correctly, built with the real `git` binary rather than with a hand-written
//! index, so the test disagrees with Git when the implementation does.

mod common;

use std::path::Path;

use common::{git, git_add_and_commit, init_git_repo, set_index_mtime, write};
use rr_core::cancel::CancelToken;
use rr_core::path::RelPath;
use rr_git::{ChangeKind, GitRepo, HeadState, RepoState, WorktreeChange};

fn observe(dir: &Path) -> RepoState {
    let repo = GitRepo::discover(dir)
        .expect("discovery failed")
        .expect("fixture is not a git repository");
    repo.observe_state(&CancelToken::new())
        .expect("observe_state failed")
}

/// The observed changes as `(kind, path, source)` triples, which is what the
/// assertions are actually about.
fn changes(state: &RepoState) -> Vec<(ChangeKind, String, Option<String>)> {
    state
        .changes
        .iter()
        .map(|change| {
            (
                change.kind,
                change.path.as_str().to_owned(),
                change.source.as_ref().map(|s| s.as_str().to_owned()),
            )
        })
        .collect()
}

fn kinds_for(state: &RepoState, path: &str) -> Vec<ChangeKind> {
    state
        .changes
        .iter()
        .filter(|change| change.path.as_str() == path)
        .map(|change| change.kind)
        .collect()
}

fn seeded() -> tempfile::TempDir {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    write(temp.path(), "src/other.rs", "pub fn two() {}\n");
    git_add_and_commit(temp.path(), "seed");
    temp
}

#[test]
fn a_committed_tree_with_nothing_touched_is_clean() {
    let temp = seeded();
    let state = observe(temp.path());

    assert!(state.is_clean(), "expected clean, got {:?}", state.changes);
    assert!(matches!(state.head, HeadState::Commit(_)));
}

/// A file written in the same second as the index has a stat Git cannot trust,
/// so Git reads its content to decide. That is correct and unavoidable — but it
/// must produce no change, and it must be visible, because it is the difference
/// between a refresh that touched nothing and one that read the whole tree.
///
/// The condition is constructed rather than waited for. A fixture that hopes to
/// commit inside one filesystem tick tests this on a coarse clock and tests
/// nothing on a fine one, and which machine it got is not something the test can
/// see. Backdating the index makes every entry racily clean at once.
#[test]
fn a_racily_clean_tree_reports_its_reads_without_inventing_changes() {
    let temp = seeded();
    set_index_mtime(temp.path(), 0, 0);

    let state = observe(temp.path());

    assert!(state.is_clean(), "expected clean, got {:?}", state.changes);
    assert!(
        state.racy_content_reads > 0,
        "every entry is newer than the index, so no stat match can be trusted"
    );
}

/// The complement, and the one the fast path depends on: when the index is
/// newer than everything it describes, every stat match is certifiable and a
/// clean tree costs no reads at all.
///
/// This is the property that makes an incremental refresh cheap. Without it the
/// no-op path still returns the right answer, but it returns it after hashing
/// the repository, and the difference never shows up as a failure — only as a
/// tool that is inexplicably slow.
#[test]
fn a_tree_older_than_its_index_is_certified_without_reading_anything() {
    let temp = seeded();
    set_index_mtime(temp.path(), 2_000_000_000, 0);

    let state = observe(temp.path());

    assert!(state.is_clean(), "expected clean, got {:?}", state.changes);
    assert_eq!(
        state.racy_content_reads, 0,
        "no entry is newer than the index, so every stat match stands on its own"
    );
}

#[test]
fn a_repository_with_no_commits_reports_an_unborn_head() {
    let temp = init_git_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    let state = observe(temp.path());

    assert_eq!(state.head, HeadState::Unborn);
    assert_eq!(kinds_for(&state, "src/lib.rs"), vec![ChangeKind::Untracked]);
}

#[test]
fn the_index_checksum_changes_only_when_the_index_does() {
    let temp = seeded();
    let before = observe(temp.path()).index_checksum;
    assert_eq!(observe(temp.path()).index_checksum, before);

    write(temp.path(), "src/new.rs", "pub fn three() {}\n");
    git(temp.path(), &["add", "src/new.rs"]);
    assert_ne!(observe(temp.path()).index_checksum, before);
}

#[test]
fn a_staged_edit_is_a_modification() {
    let temp = seeded();
    write(temp.path(), "src/lib.rs", "pub fn one() { }\n");
    git(temp.path(), &["add", "src/lib.rs"]);

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/lib.rs"),
        vec![ChangeKind::Modified]
    );
}

#[test]
fn an_unstaged_edit_is_a_modification() {
    let temp = seeded();
    write(temp.path(), "src/lib.rs", "pub fn one() { }\n");

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/lib.rs"),
        vec![ChangeKind::Modified]
    );
}

/// Both comparisons fire for the same path. Deduplication must collapse them
/// into one entry, or every downstream count is doubled for staged-then-edited
/// files — the single most common state in an active working tree.
#[test]
fn a_staged_edit_that_is_then_edited_again_is_reported_once() {
    let temp = seeded();
    write(temp.path(), "src/lib.rs", "pub fn one() { }\n");
    git(temp.path(), &["add", "src/lib.rs"]);
    write(temp.path(), "src/lib.rs", "pub fn one() {  }\n");

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/lib.rs"),
        vec![ChangeKind::Modified]
    );
}

#[test]
fn a_new_staged_file_is_an_addition_not_an_untracked_file() {
    let temp = seeded();
    write(temp.path(), "src/new.rs", "pub fn three() {}\n");
    git(temp.path(), &["add", "src/new.rs"]);

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/new.rs"),
        vec![ChangeKind::Added]
    );
}

#[test]
fn a_deleted_file_is_a_deletion_whether_staged_or_not() {
    let temp = seeded();
    std::fs::remove_file(temp.path().join("src/other.rs")).expect("remove failed");
    assert_eq!(
        kinds_for(&observe(temp.path()), "src/other.rs"),
        vec![ChangeKind::Deleted]
    );

    git(temp.path(), &["rm", "-q", "--cached", "src/other.rs"]);
    assert_eq!(
        kinds_for(&observe(temp.path()), "src/other.rs"),
        vec![ChangeKind::Deleted]
    );
}

#[test]
fn an_untracked_file_in_a_nested_directory_is_reported_by_its_full_path() {
    let temp = seeded();
    write(temp.path(), "src/deep/nested/new.rs", "pub fn four() {}\n");

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/deep/nested/new.rs"),
        vec![ChangeKind::Untracked],
        "a directory-level report would hide the file that actually changed"
    );
}

#[test]
fn an_ignored_file_is_not_a_change() {
    let temp = seeded();
    write(temp.path(), ".gitignore", "ignored/\n");
    git_add_and_commit(temp.path(), "ignore rules");
    write(temp.path(), "ignored/thing.rs", "pub fn five() {}\n");

    let state = observe(temp.path());
    assert!(
        state.is_clean(),
        "ignored paths are outside the corpus: {:?}",
        state.changes
    );
}

/// An intent-to-add entry records the empty blob as a placeholder. Classifying
/// it as anything other than a path needing content is how the empty-blob OID
/// ends up naming a real file's facts.
#[test]
fn an_intent_to_add_file_is_an_addition() {
    let temp = seeded();
    write(temp.path(), "src/new.rs", "pub fn three() {}\n");
    git(temp.path(), &["add", "-N", "src/new.rs"]);

    assert_eq!(
        kinds_for(&observe(temp.path()), "src/new.rs"),
        vec![ChangeKind::Added]
    );
}

#[test]
fn a_git_mv_is_a_rename_that_names_its_source() {
    let temp = seeded();
    git(temp.path(), &["mv", "src/other.rs", "src/renamed.rs"]);

    let state = observe(temp.path());
    let renames: Vec<_> = state
        .changes
        .iter()
        .filter(|change| change.kind == ChangeKind::Renamed)
        .collect();

    assert_eq!(
        renames,
        vec![&WorktreeChange {
            kind: ChangeKind::Renamed,
            path: RelPath::new("src/renamed.rs").expect("valid path"),
            source: Some(RelPath::new("src/other.rs").expect("valid path")),
        }]
    );
}

/// A rename whose source is immediately recreated is a legal, reachable state.
/// Both facts must survive: the target is new content, and the source is a
/// different file that happens to share the old name.
#[test]
fn a_rename_source_that_is_recreated_reports_both_paths() {
    let temp = seeded();
    git(temp.path(), &["mv", "src/other.rs", "src/renamed.rs"]);
    write(temp.path(), "src/other.rs", "pub fn replacement() {}\n");

    let state = observe(temp.path());
    assert!(
        state
            .changes
            .iter()
            .any(|change| change.path.as_str() == "src/renamed.rs"),
        "the rename target must be reported: {:?}",
        changes(&state)
    );
    assert!(
        state
            .changes
            .iter()
            .any(|change| change.path.as_str() == "src/other.rs"),
        "the recreated source must be reported: {:?}",
        changes(&state)
    );
}

/// A copy leaves its source in place. Treating it as a rename would delete a
/// file that is still there.
#[test]
fn a_copied_file_leaves_its_source_reported_as_unchanged() {
    let temp = seeded();
    std::fs::copy(
        temp.path().join("src/other.rs"),
        temp.path().join("src/copy.rs"),
    )
    .expect("copy failed");

    let state = observe(temp.path());
    assert!(
        kinds_for(&state, "src/other.rs").is_empty(),
        "the copy source is untouched: {:?}",
        changes(&state)
    );
    assert!(
        !kinds_for(&state, "src/copy.rs").is_empty(),
        "the copy target is a new path: {:?}",
        changes(&state)
    );
}

#[test]
fn an_executable_bit_flip_is_a_modification_not_a_type_change() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let temp = seeded();
        let path = temp.path().join("src/other.rs");
        let mut perms = std::fs::metadata(&path)
            .expect("metadata failed")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod failed");

        assert_eq!(
            kinds_for(&observe(temp.path()), "src/other.rs"),
            vec![ChangeKind::Modified],
            "the bytes still parse; only the mode moved"
        );
    }
}

#[test]
fn a_file_replaced_by_a_symlink_is_a_type_change() {
    #[cfg(unix)]
    {
        let temp = seeded();
        let path = temp.path().join("src/other.rs");
        std::fs::remove_file(&path).expect("remove failed");
        std::os::unix::fs::symlink("lib.rs", &path).expect("symlink failed");

        assert_eq!(
            kinds_for(&observe(temp.path()), "src/other.rs"),
            vec![ChangeKind::TypeChanged],
            "a symlink's content is its target text, not the file it names"
        );
    }
}

#[test]
fn a_merge_conflict_is_reported_as_conflicted() {
    let temp = seeded();
    git(temp.path(), &["checkout", "-q", "-b", "side"]);
    write(temp.path(), "src/lib.rs", "pub fn side() {}\n");
    git_add_and_commit(temp.path(), "side edit");

    git(temp.path(), &["checkout", "-q", "-"]);
    write(temp.path(), "src/lib.rs", "pub fn main_line() {}\n");
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

    let state = observe(temp.path());
    assert_eq!(
        state
            .conflicted
            .iter()
            .map(RelPath::as_str)
            .collect::<Vec<_>>(),
        vec!["src/lib.rs"]
    );
    assert!(kinds_for(&state, "src/lib.rs").contains(&ChangeKind::Conflicted));
}

/// A nested repository belongs to another corpus. Walking into it would put its
/// symbols in this project's index and its churn in this project's delta.
#[test]
fn a_nested_repository_is_skipped_rather_than_entered() {
    let temp = seeded();
    let nested = temp.path().join("vendored");
    std::fs::create_dir_all(&nested).expect("mkdir failed");
    git(&nested, &["init", "-q"]);
    write(&nested, "inner.rs", "pub fn inner() {}\n");

    let state = observe(temp.path());
    assert!(
        !state
            .changes
            .iter()
            .any(|change| change.path.as_str().starts_with("vendored/")),
        "nested repository contents leaked into the delta: {:?}",
        changes(&state)
    );
    assert_eq!(state.skipped_submodules, 1);
}

/// Observation order is an implementation detail of the walk. Two observations
/// of the same tree must be equal, or the "did anything change" comparison
/// becomes a coin flip.
#[test]
fn repeated_observations_of_the_same_tree_are_identical() {
    let temp = seeded();
    write(temp.path(), "a/one.rs", "pub fn a() {}\n");
    write(temp.path(), "b/two.rs", "pub fn b() {}\n");
    write(temp.path(), "c/three.rs", "pub fn c() {}\n");
    git(temp.path(), &["add", "b/two.rs"]);
    write(temp.path(), "src/lib.rs", "pub fn edited() {}\n");

    assert_eq!(observe(temp.path()), observe(temp.path()));
}

#[test]
fn changes_are_sorted_so_the_delta_is_comparable() {
    let temp = seeded();
    for name in ["z.rs", "a.rs", "m.rs"] {
        write(temp.path(), name, "pub fn x() {}\n");
    }

    let state = observe(temp.path());
    let mut sorted = state.changes.clone();
    sorted.sort();
    assert_eq!(state.changes, sorted);
}

/// The `.rr` directory holds this tool's own output. If it reached the delta,
/// every refresh would see its own last snapshot as an untracked change and no
/// run could ever conclude that nothing happened.
#[test]
fn the_tools_own_state_directory_does_not_appear_in_the_delta() {
    let temp = seeded();
    write(
        temp.path(),
        ".rr/local/snapshot.bin",
        "not really a snapshot",
    );
    write(
        temp.path(),
        ".rr/local/facts/ab/cdef.bin",
        "not really facts",
    );

    let state = observe(temp.path());
    let leaked: Vec<_> = state
        .changes
        .iter()
        .filter(|change| change.path.as_str().starts_with(".rr/"))
        .collect();
    assert!(
        leaked.is_empty(),
        "the tool's own output must not be a repository change: {leaked:?}"
    );
}

#[test]
fn cancellation_is_an_error_rather_than_an_empty_delta() {
    let temp = seeded();
    write(temp.path(), "src/new.rs", "pub fn three() {}\n");

    let repo = GitRepo::discover(temp.path())
        .expect("discovery failed")
        .expect("fixture is not a git repository");
    let cancel = CancelToken::new();
    cancel.cancel();

    let error = repo
        .observe_state(&cancel)
        .expect_err("a cancelled scan must not look clean");
    assert!(matches!(error, rr_git::Error::Cancelled), "got {error:?}");
}

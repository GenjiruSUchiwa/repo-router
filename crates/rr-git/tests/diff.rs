//! The endpoint-and-delta matrix.
//!
//! Every fixture here is built with the real `git` binary, so a comparison that
//! disagrees with Git is a failing test rather than a plausible-looking one. The
//! assertions are about three things a caller cannot recover afterwards: which
//! paths a comparison claims changed, which line ranges it attributes the change
//! to, and which paths it refuses to describe at all.

mod common;

use std::path::Path;
use std::process::Command;

use common::{git, git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::lang::Lang;
use rr_core::path::RelPath;
use rr_core::walk::WalkCfg;
use rr_git::diff::{resolve_target, worktree_target};
use rr_git::{change_set, ChangeKind, ChangeSet, FileDelta, GitRepo, Hunk};

/// Five one-line definitions, so a hunk that covers one of them cannot also be
/// the whole file by accident.
const FIVE_LINES: &str =
    "pub fn one() {}\npub fn two() {}\npub fn three() {}\npub fn four() {}\npub fn five() {}\n";

fn repo_at(dir: &Path) -> GitRepo {
    GitRepo::discover(dir)
        .expect("discovery failed")
        .expect("fixture is not a git repository")
}

/// One committed revision against the working tree, under the default walk.
fn against_worktree(dir: &Path, base: &str) -> ChangeSet {
    with_walk(dir, base, &WalkCfg::default())
}

/// One committed revision against the working tree, under a named walk.
fn with_walk(dir: &Path, base: &str, walk: &WalkCfg) -> ChangeSet {
    let repo = repo_at(dir);
    let base = resolve_target(&repo, base).expect("base revision must resolve");
    let target = worktree_target(&repo).expect("worktree endpoint must resolve");
    change_set(&repo, &base, &target, walk, &CancelToken::new()).expect("change_set failed")
}

/// Two committed revisions against each other.
fn between(dir: &Path, base: &str, target: &str) -> ChangeSet {
    let repo = repo_at(dir);
    let base = resolve_target(&repo, base).expect("base revision must resolve");
    let target = resolve_target(&repo, target).expect("target revision must resolve");
    change_set(
        &repo,
        &base,
        &target,
        &WalkCfg::default(),
        &CancelToken::new(),
    )
    .expect("change_set failed")
}

/// The changed paths, which is what most assertions here are actually about.
fn paths(set: &ChangeSet) -> Vec<&str> {
    set.files.iter().map(|file| file.path.as_str()).collect()
}

#[track_caller]
fn delta<'a>(set: &'a ChangeSet, path: &str) -> &'a FileDelta {
    set.files
        .iter()
        .find(|file| file.path.as_str() == path)
        .unwrap_or_else(|| panic!("no delta for {path}; the set holds {:?}", paths(set)))
}

fn write_bytes(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(&path, bytes).expect("failed to write file");
}

/// Runs a merge that must fail, leaving unmerged index stages behind.
///
/// `common::git` asserts success, which a conflicting merge deliberately does
/// not have; a fixture that swallowed the failure would test a clean tree.
fn merge_expecting_a_conflict(dir: &Path, branch: &str) {
    let output = Command::new("git")
        .args(["merge", "--no-edit", branch])
        .current_dir(dir)
        .output()
        .expect("failed to run git merge");
    assert!(
        !output.status.success(),
        "the fixture merge was supposed to conflict: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn tree_to_tree_reports_added_modified_deleted() {
    let temp = init_git_repo();
    write(temp.path(), "src/kept.rs", FIVE_LINES);
    write(
        temp.path(),
        "src/gone.rs",
        "pub struct Gone;\nimpl Gone {\n    pub fn vanish(&self) {}\n}\n",
    );
    git_add_and_commit(temp.path(), "base");
    write(
        temp.path(),
        "src/kept.rs",
        &FIVE_LINES.replace("pub fn three() {}", "pub fn three(now: u8) {}"),
    );
    std::fs::remove_file(temp.path().join("src/gone.rs")).expect("failed to remove file");
    write(
        temp.path(),
        "src/new.rs",
        "pub enum Fresh {\n    First,\n    Second,\n}\n",
    );
    git_add_and_commit(temp.path(), "target");

    let set = between(temp.path(), "HEAD~1", "HEAD");

    assert_eq!(paths(&set), ["src/gone.rs", "src/kept.rs", "src/new.rs"]);
    let added = delta(&set, "src/new.rs");
    assert_eq!(added.kind, ChangeKind::Added);
    assert!(added.base_oid.is_none(), "an addition has no base side");
    let modified = delta(&set, "src/kept.rs");
    assert_eq!(modified.kind, ChangeKind::Modified);
    assert_ne!(modified.base_oid, modified.target_oid);
    let deleted = delta(&set, "src/gone.rs");
    assert_eq!(deleted.kind, ChangeKind::Deleted);
    assert!(
        deleted.target_oid.is_none(),
        "a deletion has no target side"
    );
    assert!(set.raced.is_empty());
    assert!(set.conflicted.is_empty());
}

/// A pure rename is one entry with no hunks, not a deletion plus an addition:
/// the definitions moved, and nothing about them changed.
#[test]
fn rename_without_content_change_has_no_hunks() {
    let temp = init_git_repo();
    write(temp.path(), "src/before.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    git(temp.path(), &["mv", "src/before.rs", "src/after.rs"]);
    git_add_and_commit(temp.path(), "target");

    let set = between(temp.path(), "HEAD~1", "HEAD");

    assert_eq!(paths(&set), ["src/after.rs"]);
    let moved = delta(&set, "src/after.rs");
    assert_eq!(moved.kind, ChangeKind::Renamed);
    assert_eq!(
        moved.source.as_ref().map(RelPath::as_str),
        Some("src/before.rs")
    );
    assert_eq!(moved.base_oid, moved.target_oid, "the bytes did not move");
    assert!(moved.hunks.is_empty());
}

#[test]
fn worktree_target_includes_staged_and_unstaged() {
    let temp = init_git_repo();
    write(temp.path(), "src/staged.rs", FIVE_LINES);
    write(temp.path(), "src/unstaged.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    write(
        temp.path(),
        "src/staged.rs",
        &FIVE_LINES.replace("pub fn one() {}", "pub fn one(now: u8) {}"),
    );
    git(temp.path(), &["add", "src/staged.rs"]);
    write(
        temp.path(),
        "src/unstaged.rs",
        &FIVE_LINES.replace("pub fn five() {}", "pub fn five(now: u8) {}"),
    );

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(paths(&set), ["src/staged.rs", "src/unstaged.rs"]);
    assert_eq!(
        delta(&set, "src/staged.rs").hunks,
        [Hunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1
        }]
    );
    assert_eq!(
        delta(&set, "src/unstaged.rs").hunks,
        [Hunk {
            old_start: 5,
            old_lines: 1,
            new_start: 5,
            new_lines: 1
        }]
    );
}

/// Untracked eligibility is the walk's answer, not this module's: whatever
/// discovery would collect is what a comparison reports, so impact and map
/// cannot disagree about what a source file is.
#[test]
fn worktree_target_includes_eligible_untracked_only() {
    let temp = init_git_repo();
    write(temp.path(), "src/tracked.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    write(temp.path(), "src/fresh.rs", "pub fn fresh() {}\n");
    write(temp.path(), "notes.txt", "not any language\n");
    write(
        temp.path(),
        "page.md",
        "# a language the default walk collects\n",
    );

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(paths(&set), ["page.md", "src/fresh.rs"]);
    assert_eq!(delta(&set, "src/fresh.rs").kind, ChangeKind::Added);

    let rust_only = WalkCfg {
        languages: Some(vec![Lang::Rust]),
        ..WalkCfg::default()
    };
    let set = with_walk(temp.path(), "HEAD", &rust_only);

    assert_eq!(paths(&set), ["src/fresh.rs"]);
}

/// `--base <older>` with no `--head` compares that commit with the working
/// tree, so a file committed since then is part of the comparison even though
/// `git status` reports nothing whatsoever about it.
#[test]
fn an_older_base_against_the_worktree_includes_committed_changes() {
    let temp = init_git_repo();
    write(temp.path(), "src/committed.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    write(
        temp.path(),
        "src/committed.rs",
        &FIVE_LINES.replace("pub fn four() {}", "pub fn four(now: u8) {}"),
    );
    git_add_and_commit(temp.path(), "since");
    write(temp.path(), "src/dirty.rs", "pub fn dirty() {}\n");

    let set = against_worktree(temp.path(), "HEAD~1");

    assert_eq!(paths(&set), ["src/committed.rs", "src/dirty.rs"]);
    assert_eq!(
        delta(&set, "src/committed.rs").hunks,
        [Hunk {
            old_start: 4,
            old_lines: 1,
            new_start: 4,
            new_lines: 1
        }]
    );
    assert_eq!(delta(&set, "src/dirty.rs").kind, ChangeKind::Added);
}

#[test]
fn unmerged_path_lands_in_conflicted_not_files() {
    let temp = init_git_repo();
    git(temp.path(), &["checkout", "-q", "-b", "trunk"]);
    write(temp.path(), "src/both.rs", "pub fn base() {}\n");
    git_add_and_commit(temp.path(), "base");
    git(temp.path(), &["checkout", "-q", "-b", "side"]);
    write(temp.path(), "src/both.rs", "pub fn side() {}\n");
    git_add_and_commit(temp.path(), "side");
    git(temp.path(), &["checkout", "-q", "trunk"]);
    write(temp.path(), "src/both.rs", "pub fn trunk() {}\n");
    git_add_and_commit(temp.path(), "trunk");
    merge_expecting_a_conflict(temp.path(), "side");

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(
        set.conflicted
            .iter()
            .map(RelPath::as_str)
            .collect::<Vec<_>>(),
        ["src/both.rs"]
    );
    assert!(
        !paths(&set).contains(&"src/both.rs"),
        "an unmerged path has no single base side to diff against"
    );
}

/// A path whose bytes differ between the observation and the read is reported
/// as raced and described by nothing.
///
/// The race is constructed rather than waited for. A clean filter whose output
/// changes on every invocation is exactly what a file being rewritten under the
/// reader looks like from the reader's side, and unlike a sleep it produces the
/// same test on every machine.
#[cfg(unix)]
#[test]
fn content_changed_after_observation_is_raced() {
    let temp = init_git_repo();
    write(temp.path(), "src/moving.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");

    let tick = temp.path().join("tick");
    std::fs::write(&tick, b"").expect("failed to seed the tick file");
    let driver = format!(
        "cat >/dev/null; wc -c < {tick}; printf x >> {tick}",
        tick = tick.display()
    );
    git(temp.path(), &["config", "filter.tick.clean", &driver]);
    write(temp.path(), ".gitattributes", "*.rs filter=tick\n");
    write(
        temp.path(),
        "src/moving.rs",
        &FIVE_LINES.replace("pub fn two() {}", "pub fn two(now: u8) {}"),
    );

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(
        set.raced.iter().map(RelPath::as_str).collect::<Vec<_>>(),
        ["src/moving.rs"]
    );
    assert!(
        !paths(&set).contains(&"src/moving.rs"),
        "a delta over two versions of one file is not a delta of anything"
    );
}

#[test]
fn zero_context_hunk_covers_only_edited_lines() {
    let temp = init_git_repo();
    write(temp.path(), "src/five.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    write(
        temp.path(),
        "src/five.rs",
        &FIVE_LINES.replace("pub fn three() {}", "pub fn three(now: u8) {}"),
    );

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(
        delta(&set, "src/five.rs").hunks,
        [Hunk {
            old_start: 3,
            old_lines: 1,
            new_start: 3,
            new_lines: 1
        }],
        "three context lines would have covered four of the five definitions"
    );
}

#[test]
fn pure_insertion_has_zero_old_lines() {
    let temp = init_git_repo();
    write(temp.path(), "src/five.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    write(
        temp.path(),
        "src/five.rs",
        &FIVE_LINES.replace(
            "pub fn three() {}",
            "pub fn inserted() {}\npub fn three() {}",
        ),
    );

    let set = against_worktree(temp.path(), "HEAD");

    assert_eq!(
        delta(&set, "src/five.rs").hunks,
        [Hunk {
            old_start: 2,
            old_lines: 0,
            new_start: 3,
            new_lines: 1
        }],
        "a zero-width base side names the line the insertion follows"
    );
}

#[test]
fn unresolvable_revision_is_an_error_not_a_panic() {
    let temp = init_git_repo();
    write(temp.path(), "src/one.rs", FIVE_LINES);
    git_add_and_commit(temp.path(), "base");
    let repo = repo_at(temp.path());

    for spec in ["no-such-branch", "HEAD..HEAD", "HEAD...HEAD", "HEAD^{tree}"] {
        let error = resolve_target(&repo, spec)
            .expect_err("a spec that names no commit must not resolve")
            .to_string();
        assert!(
            error.contains(&format!("unresolvable revision: {spec}")),
            "{error}"
        );
    }
    assert!(
        resolve_target(&repo, "HEAD").is_ok(),
        "the fixture's own HEAD must still resolve"
    );
}

#[test]
fn binary_file_yields_no_hunks_but_a_delta() {
    let temp = init_git_repo();
    write_bytes(temp.path(), "src/blob.rs", b"\xff\xfe\x00one\n");
    git_add_and_commit(temp.path(), "base");
    write_bytes(temp.path(), "src/blob.rs", b"\xff\xfe\x00two\n");
    git_add_and_commit(temp.path(), "target");

    let set = between(temp.path(), "HEAD~1", "HEAD");

    let changed = delta(&set, "src/blob.rs");
    assert_eq!(changed.kind, ChangeKind::Modified);
    assert!(changed.base_oid.is_some());
    assert!(changed.target_oid.is_some());
    assert_ne!(changed.base_oid, changed.target_oid);
    assert!(
        changed.hunks.is_empty(),
        "line ranges over bytes no extractor can address are not evidence"
    );
}

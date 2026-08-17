//! The differential oracle over generated histories.
//!
//! The hand-written cases next door each name a scenario somebody thought of.
//! This one exists for the scenarios nobody did: sequences of ordinary edits,
//! in orders no one would choose, run until the incremental result and the full
//! rebuild disagree — which they must never do.
//!
//! Two roots rather than one. Refreshing the same directory twice lets the
//! rebuild start from whatever the incremental run left on disk, so a
//! difference in the state directory could cancel out a difference in the
//! snapshot. Copying the repository first removes that: the full rebuild sees
//! the working tree and nothing else this tool ever wrote.
//!
//! The seed is fixed. A property test that picks a new seed every run reports
//! its failures to whoever happened to be watching, and reproducing one becomes
//! an archaeology problem instead of a rerun.

mod common;

use std::path::Path;
use std::process::Command;

use common::{git, git_add_and_commit, init_git_repo, write};
use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, TestRng, TestRunner};
use rr_core::cancel::CancelToken;
use rr_core::refresh::RefreshMode;
use rr_core::snapshot::SnapshotStore;

const THREADS: usize = 2;

/// The files a generated history is allowed to touch.
const FILES: [&str; 4] = [
    "src/lib.rs",
    "src/other.rs",
    "src/nested/deep.rs",
    "src/fourth.rs",
];

/// One thing a person does to a repository.
///
/// Deliberately small and deliberately ordinary: the interesting failures come
/// from the order and the overlap, not from any single exotic operation.
#[derive(Debug, Clone)]
enum Op {
    Edit(usize),
    Delete(usize),
    Rename(usize, usize),
    Stage,
    Commit,
    Ignore(usize),
    Untracked(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let file = 0usize..FILES.len();
    prop_oneof![
        3 => file.clone().prop_map(Op::Edit),
        1 => file.clone().prop_map(Op::Delete),
        1 => (file.clone(), file.clone()).prop_map(|(a, b)| Op::Rename(a, b)),
        2 => Just(Op::Stage),
        2 => Just(Op::Commit),
        1 => file.clone().prop_map(Op::Ignore),
        1 => file.prop_map(Op::Untracked),
    ]
}

/// Applies one operation, tolerating the ones that no longer make sense.
///
/// A generated history will ask to delete a file it already deleted or rename
/// one onto itself. Skipping those keeps the generator simple and keeps the
/// sequences realistic — a person's history has the same dead ends in it.
fn apply(dir: &Path, op: &Op, step: usize) {
    let path = |i: usize| dir.join(FILES[i]);
    match op {
        Op::Edit(i) => write(dir, FILES[*i], &format!("pub fn f() -> u32 {{ {step} }}\n")),
        Op::Delete(i) => {
            let _ = std::fs::remove_file(path(*i));
        }
        Op::Rename(from, to) => {
            if from != to && path(*from).exists() && !path(*to).exists() {
                std::fs::rename(path(*from), path(*to)).expect("rename failed");
            }
        }
        Op::Stage => git(dir, &["add", "-A"]),
        Op::Commit => {
            git(dir, &["add", "-A"]);
            let _ = Command::new("git")
                .args(["commit", "-qm", "step"])
                .current_dir(dir)
                .output();
        }
        Op::Ignore(i) => write(dir, ".gitignore", &format!("{}\n", FILES[*i])),
        Op::Untracked(i) => write(
            dir,
            &format!("{}.new{step}.rs", FILES[*i]),
            "pub fn n() {}\n",
        ),
    }
}

fn seeded() -> tempfile::TempDir {
    let temp = init_git_repo();
    for (i, name) in FILES.iter().enumerate() {
        write(temp.path(), name, &format!("pub fn f() -> u32 {{ {i} }}\n"));
    }
    git_add_and_commit(temp.path(), "seed");
    rr_git::refresh(temp.path(), THREADS, RefreshMode::Full, &CancelToken::new())
        .expect("seed refresh failed");
    temp
}

/// Copies a repository so the control rebuild starts from the working tree
/// alone rather than from what the incremental run left behind.
fn duplicate(dir: &Path) -> tempfile::TempDir {
    let copy = tempfile::TempDir::new().expect("failed to create temp dir");
    let status = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", dir.display()))
        .arg(copy.path())
        .status()
        .expect("failed to copy the repository");
    assert!(status.success(), "copying the repository failed");
    copy
}

fn published(dir: &Path) -> Vec<u8> {
    SnapshotStore::new(dir)
        .read_published()
        .expect("reading the published snapshot failed")
        .expect("nothing was published")
}

/// Refreshes `dir` incrementally and an untouched copy of it in full, and
/// insists the published bytes are identical.
fn agree_isolated(dir: &Path, step: usize) {
    let control = duplicate(dir);

    rr_git::refresh(dir, THREADS, RefreshMode::Incremental, &CancelToken::new())
        .expect("incremental refresh failed");
    rr_git::refresh(
        control.path(),
        THREADS,
        RefreshMode::Full,
        &CancelToken::new(),
    )
    .expect("full refresh failed");

    let incremental = published(dir);
    let full = published(control.path());
    assert!(
        incremental == full,
        "step {step}: an incremental refresh disagreed with a full rebuild \
         ({} bytes against {})",
        incremental.len(),
        full.len()
    );
}

/// The seed, written down.
///
/// `proptest` otherwise draws a fresh one per run, which turns a failure into a
/// report nobody can reproduce — the counterexample is printed once and the next
/// run explores somewhere else entirely. Fixing it makes this test the same test
/// every time, and makes a failure a rerun rather than an investigation.
const SEED: [u8; 32] = *b"repo-router differential oracle!";

#[test]
fn any_history_of_ordinary_edits_agrees_with_a_full_rebuild() {
    let config = ProptestConfig {
        cases: 12,
        max_shrink_iters: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &SEED);
    let mut runner = TestRunner::new_with_rng(config, rng);

    let outcome = runner.run(&prop::collection::vec(op_strategy(), 1..7), |ops| {
        let temp = seeded();
        for (step, op) in ops.iter().enumerate() {
            apply(temp.path(), op, step);
            agree_isolated(temp.path(), step);
        }
        Ok(())
    });
    if let Err(error) = outcome {
        panic!("{error}");
    }
}

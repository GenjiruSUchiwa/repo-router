#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

pub fn init_git_repo() -> TempDir {
    init_git_repo_with(&[])
}

/// Initializes a repository with extra `git init` arguments.
///
/// Some repository-wide choices — the object format above all — can only be
/// made at creation, so a test that needs one cannot start from the ordinary
/// fixture and adjust it afterwards.
pub fn init_git_repo_with(extra: &[&str]) -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let mut args = vec!["init", "-q"];
    args.extend_from_slice(extra);
    git(temp.path(), &args);
    git(temp.path(), &["config", "user.name", "Test User"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    temp
}

/// Runs one `git` subcommand and fails loudly, because a fixture that silently
/// did nothing produces a passing test that proves nothing.
pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes a file, creating parent directories, so tests can name nested paths
/// without a `create_dir_all` line each.
pub fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(&path, contents).expect("failed to write file");
}

/// Sets a file's modification time to exactly this second and nanosecond.
///
/// Stat-based cleanliness is a comparison of timestamps, so a fixture that
/// wants to exercise it has to own both halves of the comparison. Waiting for
/// the clock does not: it produces whichever relationship the filesystem's
/// granularity happened to allow, which is a different test on every machine
/// and no test at all on a coarse one.
///
/// Access time is set alongside because `utimensat` sets both or neither, and
/// nothing here reads it.
pub fn set_mtime(path: &Path, secs: i64, nanos: i64) {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("cannot open {} to set its time: {error}", path.display()));
    let stamp = rustix::fs::Timespec {
        tv_sec: secs,
        tv_nsec: nanos,
    };
    rustix::fs::futimens(
        &file,
        &rustix::fs::Timestamps {
            last_access: stamp,
            last_modification: stamp,
        },
    )
    .unwrap_or_else(|error| panic!("cannot set the time of {}: {error}", path.display()));
}

/// Reads a file's modification time back, so a fixture can put it where it
/// found it.
///
/// A rewrite that restores the timestamp is the one case where stat-based
/// cleanliness has nothing left to notice, and constructing it needs the
/// original value rather than a guess at it.
#[must_use]
pub fn mtime_of(path: &Path) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("cannot stat {}: {error}", path.display()));
    (meta.mtime(), meta.mtime_nsec())
}

/// Sets the index file's own modification time, which is the other operand of
/// every racy-clean decision.
///
/// An entry is racily clean when its recorded time is not older than the index
/// that recorded it, so the index timestamp decides whether a stat match may be
/// believed. Backdate it and every entry becomes untrustworthy; advance it and
/// every entry becomes certifiable — both on demand, neither by luck.
pub fn set_index_mtime(dir: &Path, secs: i64, nanos: i64) {
    set_mtime(&dir.join(".git/index"), secs, nanos);
}

/// What `git status --porcelain` says, so a test can assert the premise that
/// Git itself sees nothing rather than assume it.
#[must_use]
pub fn porcelain(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .expect("failed to run git status");
    assert!(output.status.success(), "git status failed");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn git_add_and_commit(dir: &Path, msg: &str) {
    let add = Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .expect("git add failed");
    assert!(add.status.success(), "git add failed");

    let commit = Command::new("git")
        .args(["commit", "-qm", msg])
        .current_dir(dir)
        .output()
        .expect("git commit failed");
    assert!(commit.status.success(), "git commit failed");
}

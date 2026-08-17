//! Running `rr` against a throwaway repository.
//!
//! Each integration test file is its own binary, so anything shared has to live
//! in a module both include. Only the mechanics are here — the fixtures are
//! not, because each file wants a differently shaped repository.

#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

pub fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rr"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to execute rr binary")
}

pub fn code(output: &Output) -> i32 {
    output.status.code().expect("rr was killed by a signal")
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).expect("stdout was not one JSON object")
}

pub fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(output.status.success(), "git {args:?} failed");
}

pub fn write(root: &Path, path: &str, contents: &str) {
    let absolute = root.join(path);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent");
    }
    std::fs::write(absolute, contents).expect("failed to write file");
}

pub fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).expect("failed to read file")
}

/// An initialised repository with a committer configured and nothing in it.
pub fn empty_repo() -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["config", "user.name", "Test User"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    temp
}

/// Stages everything and commits it.
pub fn commit_all(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", message]);
}

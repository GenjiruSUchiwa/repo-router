#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn run_cmd(dir: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {program} {args:?}"
    );
}

fn setup_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    run_cmd(root, "git", &["init"]);
    run_cmd(root, "git", &["config", "user.email", "test@example.com"]);
    run_cmd(root, "git", &["config", "user.name", "Tester"]);

    let auth_dir = root.join("src").join("auth");
    fs::create_dir_all(&auth_dir).unwrap();
    fs::write(
        auth_dir.join("token.rs"),
        b"pub fn verify_token() -> bool { true }\n",
    )
    .unwrap();

    run_cmd(root, "git", &["add", "."]);
    run_cmd(root, "git", &["commit", "-m", "init"]);

    temp
}

#[test]
fn test_missing_snapshot() {
    let repo = setup_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "rr: query: index missing; run 'rr map'\n"
    );
}

#[test]
fn test_invalid_snapshot() {
    let repo = setup_repo();
    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());

    let snap_path = repo.path().join(".rr").join("local").join("snapshot.bin");
    fs::write(&snap_path, b"CORRUPTED_SNAPSHOT_PAYLOAD_GARBAGE").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "rr: query: index invalid; run 'rr map'\n"
    );
}

#[test]
fn test_stale_snapshot() {
    let repo = setup_repo();
    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());
    fs::write(
        repo.path().join("src").join("auth").join("token.rs"),
        b"pub fn verify_token() -> bool { false }\npub fn issue_token() {}\n",
    )
    .unwrap();
    run_cmd(repo.path(), "git", &["add", "."]);
    run_cmd(repo.path(), "git", &["commit", "-m", "stale_update"]);

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "rr: query: index is stale; run 'rr refresh'\n"
    );
}

/// A moved `HEAD` is not a stale index, and `rr query` must not say it is.
///
/// The commit that moves `HEAD` most often is the one that commits the
/// generated maps, which touches no indexed source at all. `rr status` has
/// answered `fresh` for that case since #44 gave it the `HEAD`-to-`HEAD` tree
/// diff; `rr query` compared commit ids and refused, so the two commands
/// contradicted each other about the same snapshot in the same second. This is
/// that contradiction, pinned shut.
#[test]
fn test_a_commit_that_changed_no_indexed_file_still_answers() {
    let repo = setup_repo();
    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());
    run_cmd(repo.path(), "git", &["add", "."]);
    run_cmd(repo.path(), "git", &["commit", "-m", "maps"]);

    let status = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    let status: serde_json::Value =
        serde_json::from_slice(&status.stdout).expect("status prints one JSON object");
    assert_eq!(status["snapshot"], "fresh");

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "status called the snapshot fresh and query refused it: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("verify_token"));
}

#[test]
fn test_source_deleted_after_index_succeeds() {
    let repo = setup_repo();
    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());

    fs::remove_file(repo.path().join("src").join("auth").join("token.rs")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_dirty_source_file_works() {
    let repo = setup_repo();
    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());

    fs::write(
        repo.path().join("src").join("auth").join("token.rs"),
        b"pub fn verify_token() -> bool { false }\npub fn modified_code() {}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token\n"
    );
    assert!(output.stderr.is_empty());
}

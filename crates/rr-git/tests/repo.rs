use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

use rr_core::path::RelPath;
use rr_git::{hash_blob, oid_of, GitRepo, HashAlgo};

fn init_git_repo() -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .output()
        .expect("failed to run git init");
    assert!(output.status.success(), "git init failed");

    // Configure user name and email for commits
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(temp.path())
        .output()
        .expect("git config user.name failed");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp.path())
        .output()
        .expect("git config user.email failed");

    temp
}

fn git_add_and_commit(dir: &std::path::Path, msg: &str) {
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

#[test]
fn discover_in_git_repo_and_subdirectories() {
    let repo_dir = init_git_repo();
    let sub = repo_dir.path().join("a").join("b");
    fs::create_dir_all(&sub).unwrap();

    let repo = GitRepo::discover(repo_dir.path()).unwrap();
    assert!(repo.is_some(), "discover root failed");

    let sub_repo = GitRepo::discover(&sub).unwrap();
    assert!(sub_repo.is_some(), "discover in subdir failed");
}

#[test]
fn discover_in_non_git_directory_returns_none() {
    let non_git = TempDir::new().unwrap();
    let repo = GitRepo::discover(non_git.path()).unwrap();
    assert!(repo.is_none());
}

#[test]
fn clean_file_index_oid_equals_hash_blob() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("src").join("main.rs");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, b"fn main() { println!(\"hello\"); }\n").unwrap();

    git_add_and_commit(repo_dir.path(), "initial commit");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("src/main.rs").unwrap();

    let index_oid = repo.index_oid(&rel).unwrap();
    assert!(index_oid.is_some(), "clean file must be found in index");

    let content = fs::read(&file_path).unwrap();
    let manual_oid = hash_blob(&content, repo.hash_algo());

    assert_eq!(index_oid.unwrap(), manual_oid);
}

#[test]
fn modified_file_index_oid_returns_none_and_oid_of_hashes_content() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("lib.rs");
    fs::write(&file_path, b"pub fn a() {}\n").unwrap();
    git_add_and_commit(repo_dir.path(), "init");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("lib.rs").unwrap();

    // Clean check
    let clean_oid = repo.index_oid(&rel).unwrap().unwrap();

    // Modify file
    fs::write(&file_path, b"pub fn a() { /* modified */ }\n").unwrap();

    // Index OID must return None for modified file
    let index_oid = repo.index_oid(&rel).unwrap();
    assert!(index_oid.is_none(), "modified file must not match index");

    // oid_of must compute new hash
    let resolved_oid = oid_of(Some(&repo), repo_dir.path(), &rel, HashAlgo::Sha1).unwrap();
    assert_ne!(resolved_oid, clean_oid);
    let expected = hash_blob(b"pub fn a() { /* modified */ }\n", HashAlgo::Sha1);
    assert_eq!(resolved_oid, expected);
}

#[test]
fn untracked_file_index_oid_returns_none_and_oid_of_hashes_content() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("untracked.txt");
    fs::write(&file_path, b"hello untracked\n").unwrap();

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("untracked.txt").unwrap();

    let index_oid = repo.index_oid(&rel).unwrap();
    assert!(index_oid.is_none(), "untracked file must not have index oid");

    let resolved = oid_of(Some(&repo), repo_dir.path(), &rel, HashAlgo::Sha1).unwrap();
    let expected = hash_blob(b"hello untracked\n", HashAlgo::Sha1);
    assert_eq!(resolved, expected);
}

#[test]
fn oid_of_without_git_repo_falls_back_to_hashing() {
    let non_git = TempDir::new().unwrap();
    let file_path = non_git.path().join("readme.md");
    fs::write(&file_path, b"# Hello\n").unwrap();

    let rel = RelPath::try_from("readme.md").unwrap();
    let resolved = oid_of(None, non_git.path(), &rel, HashAlgo::Sha1).unwrap();
    let expected = hash_blob(b"# Hello\n", HashAlgo::Sha1);
    assert_eq!(resolved, expected);
}

#[test]
fn git_mv_unmodified_file_preserves_oid() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("old_name.rs");
    fs::write(&file_path, b"fn foo() {}\n").unwrap();
    git_add_and_commit(repo_dir.path(), "add old_name");

    let repo1 = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let old_rel = RelPath::try_from("old_name.rs").unwrap();
    let old_oid = oid_of(Some(&repo1), repo_dir.path(), &old_rel, repo1.hash_algo()).unwrap();

    // Run git mv
    let mv = Command::new("git")
        .args(["mv", "old_name.rs", "new_name.rs"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git mv failed");
    assert!(mv.status.success(), "git mv failed");

    let repo2 = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let new_rel = RelPath::try_from("new_name.rs").unwrap();
    let new_oid = oid_of(Some(&repo2), repo_dir.path(), &new_rel, repo2.hash_algo()).unwrap();

    assert_eq!(old_oid, new_oid, "git mv must preserve object ID");
}

#[test]
fn property_test_multiple_committed_files_match_blob_hash() {
    let repo_dir = init_git_repo();

    let files = [
        ("src/a.rs", b"pub mod a;" as &[u8]),
        ("src/b.rs", b"pub mod b; // comment\nsecond line"),
        ("Cargo.toml", b"[package]\nname = \"test-pkg\"\n"),
        ("docs/readme.txt", b"documentation content here\n"),
    ];

    for (path, content) in &files {
        let full = repo_dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, content).unwrap();
    }

    git_add_and_commit(repo_dir.path(), "commit all files");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();

    for (path, content) in &files {
        let rel = RelPath::try_from(*path).unwrap();
        let index_oid = repo.index_oid(&rel).unwrap().expect("file must be in index");
        let memory_oid = hash_blob(content, repo.hash_algo());
        assert_eq!(index_oid, memory_oid, "mismatch for file {path}");

        let resolved_oid = oid_of(Some(&repo), repo_dir.path(), &rel, repo.hash_algo()).unwrap();
        assert_eq!(resolved_oid, memory_oid, "oid_of mismatch for {path}");
    }
}

#[test]
fn permission_denied_propagates_as_error() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("secret.rs");
    fs::write(&file_path, b"secret content").unwrap();
    git_add_and_commit(repo_dir.path(), "add secret");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    // Make file parent directory unreadable/unexecutable
    let sub = repo_dir.path().join("restricted");
    fs::create_dir(&sub).unwrap();
    let sub_file = sub.join("inner.rs");
    fs::write(&sub_file, b"inner content").unwrap();
    git_add_and_commit(repo_dir.path(), "add restricted");

    let inner_rel = RelPath::try_from("restricted/inner.rs").unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();

    let res = repo.index_oid(&inner_rel);

    // Restore permissions so cleanup works
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(res.is_err(), "permission error must propagate as Err");
    assert!(matches!(res.unwrap_err(), rr_git::Error::Io(_)));
}

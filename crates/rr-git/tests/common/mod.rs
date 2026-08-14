use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

pub fn init_git_repo() -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .output()
        .expect("failed to run git init");
    assert!(output.status.success(), "git init failed");

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

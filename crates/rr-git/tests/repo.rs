mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

use common::{git, git_add_and_commit, init_git_repo, set_index_mtime, set_mtime};
use rr_core::path::RelPath;
use rr_git::{hash_blob, oid_of, GitRepo, HashAlgo};

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

    let clean_oid = repo.index_oid(&rel).unwrap().unwrap();

    fs::write(&file_path, b"pub fn a() { /* modified */ }\n").unwrap();

    let index_oid = repo.index_oid(&rel).unwrap();
    assert!(index_oid.is_none(), "modified file must not match index");

    let resolved_oid = oid_of(Some(&repo), repo_dir.path(), &rel).unwrap();
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
    assert!(
        index_oid.is_none(),
        "untracked file must not have index oid"
    );

    let resolved = oid_of(Some(&repo), repo_dir.path(), &rel).unwrap();
    let expected = hash_blob(b"hello untracked\n", HashAlgo::Sha1);
    assert_eq!(resolved, expected);
}

#[test]
fn oid_of_without_git_repo_falls_back_to_hashing() {
    let non_git = TempDir::new().unwrap();
    let file_path = non_git.path().join("readme.md");
    fs::write(&file_path, b"# Hello\n").unwrap();

    let rel = RelPath::try_from("readme.md").unwrap();
    let resolved = oid_of(None, non_git.path(), &rel).unwrap();
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
    let old_oid = oid_of(Some(&repo1), repo_dir.path(), &old_rel).unwrap();

    let mv = Command::new("git")
        .args(["mv", "old_name.rs", "new_name.rs"])
        .current_dir(repo_dir.path())
        .output()
        .expect("git mv failed");
    assert!(mv.status.success(), "git mv failed");

    let repo2 = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let new_rel = RelPath::try_from("new_name.rs").unwrap();
    let new_oid = oid_of(Some(&repo2), repo_dir.path(), &new_rel).unwrap();

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
        let index_oid = repo
            .index_oid(&rel)
            .unwrap()
            .expect("file must be in index");
        let memory_oid = hash_blob(content, repo.hash_algo());
        assert_eq!(index_oid, memory_oid, "mismatch for file {path}");

        let resolved_oid = oid_of(Some(&repo), repo_dir.path(), &rel).unwrap();
        assert_eq!(resolved_oid, memory_oid, "oid_of mismatch for {path}");
    }
}

#[test]
fn oid_of_with_root_deeper_than_workdir_hashes_correct_file() {
    let repo_dir = init_git_repo();
    fs::write(repo_dir.path().join("lib.rs"), b"top-level\n").unwrap();
    let sub = repo_dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("lib.rs"), b"nested\n").unwrap();
    git_add_and_commit(repo_dir.path(), "add both");

    let repo = GitRepo::discover(&sub).unwrap().unwrap();
    let rel = RelPath::try_from("lib.rs").unwrap();

    let oid = oid_of(Some(&repo), &sub, &rel).unwrap();
    let expected = hash_blob(b"nested\n", repo.hash_algo());
    assert_eq!(
        oid, expected,
        "oid_of must resolve rel against the caller's root, not the repo workdir"
    );
}

#[cfg(unix)]
#[test]
fn tracked_symlink_hashes_link_target_text() {
    let repo_dir = init_git_repo();
    fs::write(repo_dir.path().join("real.rs"), b"pub fn real() {}\n").unwrap();
    std::os::unix::fs::symlink("real.rs", repo_dir.path().join("link.rs")).unwrap();
    git_add_and_commit(repo_dir.path(), "add symlink");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("link.rs").unwrap();

    let oid = oid_of(Some(&repo), repo_dir.path(), &rel).unwrap();
    let expected = hash_blob(b"real.rs", repo.hash_algo());
    assert_eq!(oid, expected, "symlink must hash its link target text");
}

#[test]
fn dirty_file_with_text_attribute_normalizes_crlf_before_hashing() {
    let repo_dir = init_git_repo();
    fs::write(repo_dir.path().join(".gitattributes"), b"*.rs text\n").unwrap();
    git_add_and_commit(repo_dir.path(), "add attributes");

    fs::write(
        repo_dir.path().join("crlf.rs"),
        b"fn a() {}\r\nfn b() {}\r\n",
    )
    .unwrap();

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("crlf.rs").unwrap();

    let oid = oid_of(Some(&repo), repo_dir.path(), &rel).unwrap();
    let expected = hash_blob(b"fn a() {}\nfn b() {}\n", repo.hash_algo());
    assert_eq!(oid, expected, "content filters must apply before hashing");
}

#[test]
fn permission_denied_propagates_as_error() {
    let repo_dir = init_git_repo();
    let file_path = repo_dir.path().join("secret.rs");
    fs::write(&file_path, b"secret content").unwrap();
    git_add_and_commit(repo_dir.path(), "add secret");

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let sub = repo_dir.path().join("restricted");
    fs::create_dir(&sub).unwrap();
    let sub_file = sub.join("inner.rs");
    fs::write(&sub_file, b"inner content").unwrap();
    git_add_and_commit(repo_dir.path(), "add restricted");

    let inner_rel = RelPath::try_from("restricted/inner.rs").unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();

    let res = repo.index_oid(&inner_rel);

    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(res.is_err(), "permission error must propagate as Err");
    assert!(matches!(res.unwrap_err(), rr_git::Error::Io(_)));
}

/// A second chosen far from the present so nothing in the fixture can drift
/// into it, and stable across runs so the tests are the same test every time.
const FIXED_SECOND: i64 = 1_700_000_000;

/// Whether a fixture leaves the change time in the comparison.
///
/// It is a separate axis from everything else these tests vary, and it has to
/// be settled before the repository is opened, because the option is read from
/// config once. Naming it beats a bare `false` at the call site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeTime {
    /// Compared, as Git does by default.
    Trusted,
    /// Excluded, as `core.trustctime=false` asks for.
    Ignored,
}

/// Prepares a repository holding one committed file whose modification time is
/// exactly `nanos` into [`FIXED_SECOND`], with an index that is strictly newer.
///
/// The index has to be newer or every entry is racily clean and the question
/// under test never gets asked — the read happens for the other reason, and the
/// test would pass while proving nothing.
///
/// The change time is the one field a fixture cannot set: writing a file moves
/// it to now, and so does restoring a timestamp. A test about the *modification*
/// time therefore has to take it out of the comparison, or it is a test about
/// whether two calls landed in the same second.
fn committed_at_nanos(nanos: i64, change_time: ChangeTime) -> (TempDir, GitRepo, RelPath) {
    let repo_dir = init_git_repo();
    if change_time == ChangeTime::Ignored {
        git(repo_dir.path(), &["config", "core.trustctime", "false"]);
    }
    let file = repo_dir.path().join("lib.rs");
    fs::write(&file, b"pub fn a() {}\n").unwrap();
    set_mtime(&file, FIXED_SECOND, nanos);

    git_add_and_commit(repo_dir.path(), "init");
    set_index_mtime(repo_dir.path(), FIXED_SECOND + 10, 0);

    let repo = GitRepo::discover(repo_dir.path()).unwrap().unwrap();
    let rel = RelPath::try_from("lib.rs").unwrap();
    (repo_dir, repo, rel)
}

/// An entry that recorded no nanosecond cannot be compared on one. Git stores a
/// zero in that field on filesystems and builds that do not report sub-second
/// times, and a zero is indistinguishable from a file genuinely modified on the
/// second — so comparing it would call clean files dirty forever.
#[test]
fn an_entry_that_recorded_no_nanosecond_certifies_across_a_nanosecond_change() {
    let (repo_dir, repo, rel) = committed_at_nanos(0, ChangeTime::Ignored);
    set_mtime(&repo_dir.path().join("lib.rs"), FIXED_SECOND, 750_000_000);

    assert!(
        repo.index_oid(&rel).unwrap().is_some(),
        "a zero nanosecond records nothing to disagree with"
    );
}

/// An entry that did record a nanosecond must be compared on it. Dropping to
/// second granularity would certify a file rewritten within the same second as
/// unchanged, which is the whole failure the field exists to prevent.
#[test]
fn an_entry_that_recorded_a_nanosecond_declines_when_it_changes() {
    let (repo_dir, repo, rel) = committed_at_nanos(500_000_000, ChangeTime::Ignored);

    set_mtime(&repo_dir.path().join("lib.rs"), FIXED_SECOND, 250_000_000);

    assert!(
        repo.index_oid(&rel).unwrap().is_none(),
        "a recorded nanosecond that no longer matches is a stat mismatch"
    );
}

/// A same-size rewrite that restores the modification time still fails to
/// certify, because the change time moved and the change time is recorded.
///
/// This is the last line of stat-based defence. Size is unchanged, modification
/// time is put back to the nanosecond, and what remains is a field the writer
/// does not control: rewriting a file moves its change time, and no `utimes`
/// call moves it back. Comparing it as recorded is what keeps a rewrite inside
/// one second from being certified as untouched.
///
/// It is defence, not proof — a repository configured not to trust change times
/// gives this up deliberately, and Git gives up the same thing at the same
/// moment. The guard here is against giving it up by accident.
#[test]
fn a_same_size_rewrite_that_restores_its_timestamp_is_not_certified() {
    let (repo_dir, repo, rel) = committed_at_nanos(500_000_000, ChangeTime::Trusted);
    let file = repo_dir.path().join("lib.rs");
    assert!(
        repo.index_oid(&rel).unwrap().is_some(),
        "the fixture must start certifiable or the assertion below is vacuous"
    );

    fs::write(&file, b"pub fn b() {}\n").unwrap();
    set_mtime(&file, FIXED_SECOND, 500_000_000);

    assert!(
        repo.index_oid(&rel).unwrap().is_none(),
        "a stat that no longer matches as recorded cannot certify anything"
    );
}

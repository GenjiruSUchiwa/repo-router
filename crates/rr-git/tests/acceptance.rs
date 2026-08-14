use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

use rr_core::cache::{CacheKey, CacheOutcome, FactCache};
use rr_core::walk::{discover, SourceFile, WalkCfg};
use rr_git::{oid_of, GitRepo, HashAlgo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExtractedFacts {
    symbol_count: usize,
    summary: String,
}

fn extract_mock_facts(content: &[u8], parse_counter: &AtomicUsize) -> ExtractedFacts {
    parse_counter.fetch_add(1, Ordering::SeqCst);
    let s = String::from_utf8_lossy(content);
    ExtractedFacts {
        symbol_count: s.lines().count(),
        summary: format!("lines: {}", s.lines().count()),
    }
}

fn init_git_repo() -> TempDir {
    let temp = TempDir::new().expect("failed to create temp dir");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .output()
        .expect("failed to run git init");
    assert!(output.status.success(), "git init failed");

    Command::new("git")
        .args(["config", "user.name", "Acceptance Tester"])
        .current_dir(temp.path())
        .output()
        .expect("git config user.name failed");
    Command::new("git")
        .args(["config", "user.email", "test@acceptance.org"])
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

fn run_pipeline(
    root: &std::path::Path,
    repo: Option<&GitRepo>,
    cache: &FactCache,
    parse_counter: &AtomicUsize,
) -> Vec<(SourceFile, ExtractedFacts)> {
    let cfg = WalkCfg::default();
    let files = discover(root, &cfg).expect("discover failed");
    let mut results = Vec::new();

    let algo = repo.map_or(HashAlgo::Sha1, GitRepo::hash_algo);

    for file in files {
        let oid = oid_of(repo, root, &file.path, algo).expect("oid_of failed");
        let key = CacheKey::new(oid, file.lang);

        let facts = match cache.get::<ExtractedFacts>(&key).expect("cache get failed") {
            CacheOutcome::Hit(f) => f,
            CacheOutcome::Miss | CacheOutcome::Corrupt => {
                let content = fs::read(root.join(file.path.as_str())).expect("read failed");
                let extracted = extract_mock_facts(&content, parse_counter);
                cache.put(&key, &extracted).expect("cache put failed");
                extracted
            }
        };

        results.push((file, facts));
    }

    results
}

#[test]
fn acceptance_criterion_1_second_run_100_percent_cache_hits_zero_parses() {
    let repo_dir = init_git_repo();
    let root = repo_dir.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn a() { 1 }\n").unwrap();
    fs::write(root.join("src/b.rs"), "pub fn b() { 2 }\n").unwrap();
    fs::write(root.join("src/c.rs"), "pub fn c() { 3 }\n").unwrap();

    git_add_and_commit(root, "initial commit");

    let repo = GitRepo::discover(root).unwrap().unwrap();
    let cache = FactCache::open(root).unwrap();
    let parse_counter = AtomicUsize::new(0);

    // First run: cold cache
    let first_results = run_pipeline(root, Some(&repo), &cache, &parse_counter);
    assert_eq!(first_results.len(), 3);
    assert_eq!(parse_counter.load(Ordering::SeqCst), 3, "3 cold parses");
    assert_eq!(cache.stats().misses(), 3);
    assert_eq!(cache.stats().hits(), 0);

    // Second run: warm cache
    let parse_counter_run2 = AtomicUsize::new(0);
    let second_results = run_pipeline(root, Some(&repo), &cache, &parse_counter_run2);
    assert_eq!(second_results.len(), 3);
    assert_eq!(
        parse_counter_run2.load(Ordering::SeqCst),
        0,
        "second run must do 0 parses"
    );
    assert_eq!(
        cache.stats().hits(),
        3,
        "second run must have 3 cache hits"
    );
    assert_eq!(first_results, second_results);
}

#[test]
fn acceptance_criterion_2_git_mv_of_unmodified_file_hits_cache() {
    let repo_dir = init_git_repo();
    let root = repo_dir.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/old_module.rs"), "pub fn compute() -> i32 { 42 }\n").unwrap();

    git_add_and_commit(root, "add module");

    let repo = GitRepo::discover(root).unwrap().unwrap();
    let cache = FactCache::open(root).unwrap();
    let parse_counter = AtomicUsize::new(0);

    // Run 1: index old_module.rs
    let _ = run_pipeline(root, Some(&repo), &cache, &parse_counter);
    assert_eq!(parse_counter.load(Ordering::SeqCst), 1);

    // Git mv old_module.rs -> new_module.rs
    let mv = Command::new("git")
        .args(["mv", "src/old_module.rs", "src/new_module.rs"])
        .current_dir(root)
        .output()
        .expect("git mv failed");
    assert!(mv.status.success(), "git mv failed");

    // Run 2: re-discover and run pipeline
    let parse_counter_after_mv = AtomicUsize::new(0);
    let repo2 = GitRepo::discover(root).unwrap().unwrap();
    let after_mv_results = run_pipeline(root, Some(&repo2), &cache, &parse_counter_after_mv);

    assert_eq!(after_mv_results.len(), 1);
    assert_eq!(after_mv_results[0].0.path.as_str(), "src/new_module.rs");
    assert_eq!(
        parse_counter_after_mv.load(Ordering::SeqCst),
        0,
        "git mv of unmodified file must hit cache (0 parses)"
    );
}

#[test]
fn acceptance_criterion_3_editing_a_file_only_reparses_that_file() {
    let repo_dir = init_git_repo();
    let root = repo_dir.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/file1.rs"), "pub fn f1() {}\n").unwrap();
    fs::write(root.join("src/file2.rs"), "pub fn f2() {}\n").unwrap();
    fs::write(root.join("src/file3.rs"), "pub fn f3() {}\n").unwrap();

    git_add_and_commit(root, "commit 3 files");

    let repo = GitRepo::discover(root).unwrap().unwrap();
    let cache = FactCache::open(root).unwrap();
    let parse_counter = AtomicUsize::new(0);

    // Run 1: cold cache
    let _ = run_pipeline(root, Some(&repo), &cache, &parse_counter);
    assert_eq!(parse_counter.load(Ordering::SeqCst), 3);

    // Modify only file2.rs
    fs::write(root.join("src/file2.rs"), "pub fn f2() { /* modified */ }\n").unwrap();

    // Run 2: pipeline with edited file
    let parse_counter_run2 = AtomicUsize::new(0);
    let results_run2 = run_pipeline(root, Some(&repo), &cache, &parse_counter_run2);

    assert_eq!(results_run2.len(), 3);
    assert_eq!(
        parse_counter_run2.load(Ordering::SeqCst),
        1,
        "editing 1 file must cause exactly 1 re-parse"
    );
}

#[test]
fn acceptance_criterion_4_works_in_directory_without_git() {
    let non_git = TempDir::new().unwrap();
    let root = non_git.path();

    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join("pkg/lib.rs"), "pub struct Core;\n").unwrap();
    fs::write(root.join("pkg/util.rs"), "pub fn util() {}\n").unwrap();

    let repo = GitRepo::discover(root).unwrap();
    assert!(repo.is_none(), "no git repository must be discovered");

    let cache = FactCache::open(root).unwrap();
    let parse_counter = AtomicUsize::new(0);

    // Run 1: non-git directory indexing
    let first_results = run_pipeline(root, repo.as_ref(), &cache, &parse_counter);
    assert_eq!(first_results.len(), 2);
    assert_eq!(parse_counter.load(Ordering::SeqCst), 2);

    // Run 2: non-git directory cache hits
    let parse_counter_run2 = AtomicUsize::new(0);
    let second_results = run_pipeline(root, repo.as_ref(), &cache, &parse_counter_run2);
    assert_eq!(second_results.len(), 2);
    assert_eq!(
        parse_counter_run2.load(Ordering::SeqCst),
        0,
        "warm non-git directory must hit cache (0 parses)"
    );
    assert_eq!(first_results, second_results);
}

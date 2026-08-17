//! The co-change bounds, one fixture per bound.
//!
//! Every fixture is built with the real `git` binary, so a claim about history
//! is a claim about what Git recorded rather than about what this crate believes
//! it recorded. The assertions are deliberately about *counts* and not only
//! about presence: a bound that let one extra commit through would still report
//! the same pair, and only the count says so.
//!
//! Two fixture facts are load-bearing everywhere below. The bulk-rewrite cut-off
//! is a share of the eligible corpus, so every repository here holds ten source
//! files — a two-path commit is exactly twenty percent of ten, and exactly is
//! inside the bound. And the initial commit introduces all ten at once, which is
//! a hundred percent: it is a bulk rewrite and is skipped, which is what leaves
//! every count below equal to the number of edits a test actually made.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{git, git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::path::RelPath;
use rr_core::snapshot::SNAPSHOT_MAGIC;
use rr_core::walk::WalkCfg;
use rr_git::cochange::{co_changed, CoChange, COCHANGE_CONFIG_VERSION, MIN_JACCARD_PPM};
use rr_git::diff::resolve_target;
use rr_git::GitRepo;
use tempfile::TempDir;

/// Source files every fixture holds, so a two-path commit stays inside the
/// twenty-percent share.
const ELIGIBLE_FLOOR: usize = 10;

/// A committed repository holding `paths`, padded to [`ELIGIBLE_FLOOR`] sources.
fn repo_with(paths: &[&str]) -> TempDir {
    let temp = init_git_repo();
    for path in paths {
        write(temp.path(), path, "pub fn one() {}\n");
    }
    for pad in paths.len()..ELIGIBLE_FLOOR {
        write(temp.path(), &format!("pad{pad}.rs"), "pub fn pad() {}\n");
    }
    git_add_and_commit(temp.path(), "initial");
    temp
}

/// Rewrites every path in `paths` and commits them in one commit, which is one
/// shared commit for every pair among them.
///
/// `generation` only has to differ from the last one: identical bytes are not a
/// change, and `git commit` refuses an empty commit — a fixture that reused the
/// same content would assert against history it never wrote.
fn touch(dir: &Path, paths: &[&str], generation: usize) {
    for path in paths {
        write(
            dir,
            path,
            &format!("pub fn one() {{}}\npub fn v{generation}() {{}}\n"),
        );
    }
    git_add_and_commit(dir, "edit");
}

/// The co-change evidence for `seeds`, read from `HEAD`.
fn evidence(dir: &Path, seeds: &[&str]) -> BTreeMap<RelPath, CoChange> {
    let repo = GitRepo::discover(dir)
        .expect("discovery failed")
        .expect("fixture is not a git repository");
    let target = resolve_target(&repo, "HEAD").expect("HEAD must resolve");
    let seeds: Vec<RelPath> = seeds
        .iter()
        .map(|seed| RelPath::new(seed).expect("seed must be a relative path"))
        .collect();
    co_changed(
        &repo,
        &target,
        &seeds,
        &WalkCfg::default(),
        &CancelToken::new(),
    )
    .expect("co_changed failed")
}

#[track_caller]
fn reported<'a>(found: &'a BTreeMap<RelPath, CoChange>, path: &str) -> &'a CoChange {
    let key = RelPath::new(path).expect("path must be relative");
    found.get(&key).unwrap_or_else(|| {
        panic!(
            "{path} is not reported; the evidence holds {:?}",
            keys(found)
        )
    })
}

#[track_caller]
fn assert_absent(found: &BTreeMap<RelPath, CoChange>, path: &str, why: &str) {
    let key = RelPath::new(path).expect("path must be relative");
    assert!(
        !found.contains_key(&key),
        "{path} must not be reported ({why}); the evidence holds {:?}",
        keys(found)
    );
}

fn keys(found: &BTreeMap<RelPath, CoChange>) -> Vec<&str> {
    found.keys().map(RelPath::as_str).collect()
}

/// The cache file this crate writes, named through the owning layout module.
fn cache_file(dir: &Path) -> std::path::PathBuf {
    rr_core::workspace::local_dir(dir).join("cochange.bin")
}

fn cache_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(cache_file(dir)).expect("the cold run must leave a cache behind")
}

/// The evidence rendered as JSON, which is the byte-for-byte comparison two runs
/// are held to: it fixes the key order and the six-decimal ratio at once.
fn rendered(found: &BTreeMap<RelPath, CoChange>) -> String {
    let by_path: BTreeMap<&str, &CoChange> = found
        .iter()
        .map(|(path, value)| (path.as_str(), value))
        .collect();
    serde_json::to_string(&by_path).expect("evidence must render as JSON")
}

#[test]
fn pair_below_three_shared_commits_is_not_reported() {
    let temp = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..2 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let found = evidence(temp.path(), &["a.rs"]);
    assert_absent(&found, "b.rs", "two shared commits are a coincidence");
}

#[test]
fn pair_at_exactly_three_shared_commits_is_reported() {
    let temp = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let found = evidence(temp.path(), &["a.rs"]);
    assert_eq!(
        reported(&found, "b.rs"),
        &CoChange {
            together: 3,
            commits_a: 3,
            commits_b: 3,
            jaccard_ppm: 1_000_000,
        }
    );
}

#[test]
fn jaccard_just_below_and_at_the_floor() {
    let at_the_floor = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(at_the_floor.path(), &["a.rs", "b.rs"], generation);
    }
    for generation in 3..10 {
        touch(at_the_floor.path(), &["b.rs"], generation);
    }
    let found = evidence(at_the_floor.path(), &["a.rs"]);
    let value = reported(&found, "b.rs");
    assert_eq!(
        (value.together, value.commits_a, value.commits_b),
        (3, 3, 10),
        "three shared commits out of a union of ten"
    );
    assert_eq!(
        value.jaccard_ppm, MIN_JACCARD_PPM,
        "a ratio exactly at the floor is inside it"
    );

    let below = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(below.path(), &["a.rs", "b.rs"], generation);
    }
    for generation in 3..11 {
        touch(below.path(), &["b.rs"], generation);
    }
    let found = evidence(below.path(), &["a.rs"]);
    assert_absent(
        &found,
        "b.rs",
        "one more solo commit puts the union at eleven",
    );
}

#[test]
fn merge_commits_are_skipped() {
    let temp = repo_with(&["a.rs", "b.rs", "d.rs", "e.rs"]);
    for generation in 0..2 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }
    for generation in 2..5 {
        touch(temp.path(), &["d.rs", "e.rs"], generation);
    }

    git(temp.path(), &["checkout", "-q", "-b", "side"]);
    touch(temp.path(), &["a.rs"], 5);
    touch(temp.path(), &["b.rs"], 6);
    git(temp.path(), &["checkout", "-q", "-"]);
    git(
        temp.path(),
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    let found = evidence(temp.path(), &["a.rs", "d.rs"]);
    assert_absent(
        &found,
        "b.rs",
        "the merge would have been the third shared commit",
    );
    assert_eq!(
        reported(&found, "e.rs").together,
        3,
        "the pair made of ordinary commits is still reported"
    );
}

#[test]
fn bulk_commit_over_one_thousand_paths_is_ignored() {
    let temp = init_git_repo();
    write(temp.path(), "a.rs", "pub fn one() {}\n");
    write(temp.path(), "b.rs", "pub fn one() {}\n");
    for bulk in 0..5_008 {
        write(
            temp.path(),
            &format!("bulk/f{bulk}.rs"),
            "pub fn bulk() {}\n",
        );
    }
    git_add_and_commit(temp.path(), "a corpus of five thousand sources");
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let mut wide: Vec<String> = (0..1_000).map(|bulk| format!("bulk/f{bulk}.rs")).collect();
    wide.push(String::from("a.rs"));
    for path in &wide {
        write(temp.path(), path, "pub fn bulk() {}\npub fn again() {}\n");
    }
    git_add_and_commit(temp.path(), "a thousand and one paths");

    let found = evidence(temp.path(), &["a.rs"]);
    let value = reported(&found, "b.rs");
    assert_eq!(
        (value.together, value.commits_a),
        (3, 3),
        "the thousand-and-one-path commit is under a fifth of the corpus and is still a rewrite"
    );
}

#[test]
fn bulk_commit_over_twenty_percent_of_eligible_files_is_ignored() {
    let temp = repo_with(&["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs", "c.rs"], generation);
    }
    for generation in 3..6 {
        touch(temp.path(), &["d.rs", "e.rs"], generation);
    }

    let found = evidence(temp.path(), &["a.rs", "d.rs"]);
    assert_absent(
        &found,
        "b.rs",
        "three paths of ten is over the share, so no commit pairs a with b",
    );
    assert_absent(&found, "c.rs", "the same commits are the only evidence");
    assert_eq!(
        reported(&found, "e.rs").together,
        3,
        "two paths of ten is exactly the share, which is inside it"
    );
}

#[test]
fn only_the_newest_fifty_commits_are_read() {
    let temp = repo_with(&["a.rs", "b.rs", "x.rs", "y.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }
    for filler in 0..47 {
        write(
            temp.path(),
            &format!("filler/f{filler}.txt"),
            "not a source\n",
        );
        git_add_and_commit(temp.path(), "filler");
    }
    for generation in 3..6 {
        touch(temp.path(), &["x.rs", "y.rs"], generation);
    }

    let found = evidence(temp.path(), &["a.rs", "x.rs"]);
    assert_eq!(
        reported(&found, "y.rs").together,
        3,
        "the pair inside the window is reported"
    );
    assert_absent(
        &found,
        "b.rs",
        "the fifty newest commits stop before the pair that would prove it",
    );
}

#[test]
fn cache_hit_matches_cold_computation_byte_for_byte() {
    let temp = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let cold = evidence(temp.path(), &["a.rs"]);
    let written = cache_bytes(temp.path());
    let warm = evidence(temp.path(), &["a.rs"]);

    assert_eq!(rendered(&warm), rendered(&cold));
    assert_eq!(
        cache_bytes(temp.path()),
        written,
        "a run that accepted the cache has nothing to write"
    );
}

#[test]
fn trailing_bytes_in_the_cache_force_recomputation() {
    let temp = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let cold = evidence(temp.path(), &["a.rs"]);
    let written = cache_bytes(temp.path());
    let mut tampered = written.clone();
    tampered.extend_from_slice(b"trailing");
    std::fs::write(cache_file(temp.path()), &tampered).expect("the cache must be writable");

    let after = evidence(temp.path(), &["a.rs"]);
    assert_eq!(rendered(&after), rendered(&cold));
    assert_eq!(
        cache_bytes(temp.path()),
        written,
        "the refused file was recomputed and rewritten without the trailing bytes"
    );
}

#[test]
fn config_version_bump_invalidates_the_cache() {
    let temp = repo_with(&["a.rs", "b.rs"]);
    for generation in 0..3 {
        touch(temp.path(), &["a.rs", "b.rs"], generation);
    }

    let cold = evidence(temp.path(), &["a.rs"]);
    let written = cache_bytes(temp.path());
    let mut tampered = written.clone();
    let version = SNAPSHOT_MAGIC.len();
    tampered[version..version + 4].copy_from_slice(&(COCHANGE_CONFIG_VERSION + 1).to_le_bytes());
    std::fs::write(cache_file(temp.path()), &tampered).expect("the cache must be writable");

    let after = evidence(temp.path(), &["a.rs"]);
    assert_eq!(rendered(&after), rendered(&cold));
    assert_eq!(
        cache_bytes(temp.path()),
        written,
        "a file written under other rules is replaced, not believed"
    );
}

//! The published contract of `rr refresh`, `rr map`, and `rr status`.
//!
//! Exit codes and JSON keys are the part of a CLI that other programs depend
//! on, so they are pinned here rather than left to whatever the renderers
//! happen to emit. The distinction the tests care about most is `2` — the
//! command worked and the answer is not one to act on — staying separate from
//! `1`, which means it did not work at all.

mod common;

use serde_json::Value;
use tempfile::TempDir;

use common::{code, commit_all, empty_repo, git, json, run, stdout, write};

/// A repository with one committed source file and no snapshot.
fn repo() -> TempDir {
    let temp = empty_repo();
    write(temp.path(), "src/lib.rs", "pub fn one() {}\n");
    commit_all(temp.path(), "seed");
    temp
}

/// Producing the report is the job, so producing one is success.
///
/// The verdict travels in the report, where it can name five states. An exit
/// code could distinguish two, and would spend the `2` that a mistyped flag
/// already owns — leaving a caller unable to tell a stale index from a typo.
#[test]
fn status_succeeds_whatever_it_has_to_report() {
    let temp = repo();

    let before = run(temp.path(), &["status"]);
    assert_eq!(
        code(&before),
        0,
        "reporting a missing snapshot is not a failure"
    );
    assert!(stdout(&before).contains("snapshot: missing"));

    assert_eq!(code(&run(temp.path(), &["map"])), 0);

    let after = run(temp.path(), &["status"]);
    assert_eq!(code(&after), 0);
    assert!(stdout(&after).contains("snapshot: fresh"));
}

#[test]
fn status_json_carries_the_versioned_contract() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let value = json(&run(temp.path(), &["status", "--json"]));

    assert_eq!(value["schema_version"], 3);
    assert_eq!(value["command"], "status");
    // A generation writes committed maps, so it leaves the tree dirty until the
    // user commits them. `dirty` here is the artifacts, not stale work.
    assert_eq!(value["git"], "dirty");
    assert_eq!(value["snapshot"], "fresh");
    assert!(value["head"].is_string());
    assert!(value["unresolved"].is_u64());
}

/// Committing the maps costs the snapshot its fast path.
///
/// The delta is `git status` — the working tree against `HEAD` — so a snapshot
/// is only incrementally usable while `HEAD` stands still. Committing the maps
/// moves it, which makes the next refresh a full fallback for every user of
/// every repository. Pinned rather than hidden: the fallback still converges
/// off the cache and writes nothing, and the day a `HEAD`-to-`HEAD` diff makes
/// this incremental, this test is where that shows up.
#[test]
fn committing_the_generated_maps_moves_head_and_forces_a_full_fallback() {
    let temp = repo();
    run(temp.path(), &["map"]);
    commit_all(temp.path(), "maps");

    let status = json(&run(temp.path(), &["status", "--json"]));
    assert_eq!(status["git"], "clean");
    assert_eq!(status["snapshot"], "stale");

    let refreshed = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(refreshed["mode"], "fallback-full");
    assert_eq!(refreshed["fallback_reason"], "head-changed");
    // A full fallback is a full walk, not a full reparse, and it finds the
    // artifacts already correct.
    assert_eq!(refreshed["reparsed"], 0);
    assert_eq!(refreshed["cached"], 1);

    let after = json(&run(temp.path(), &["status", "--json"]));
    assert_eq!(after["snapshot"], "fresh");
    assert_eq!(
        after["git"], "clean",
        "the fallback rewrote a committed map"
    );
}

#[test]
fn refresh_json_reports_the_mode_it_actually_ran_in() {
    let temp = repo();
    run(temp.path(), &["map"]);
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn one() {}\npub fn two() {}\n",
    )
    .expect("failed to edit source");

    let incremental = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(incremental["command"], "refresh");
    assert_eq!(incremental["mode"], "incremental");
    assert_eq!(incremental["outcome"], "updated");
    assert_eq!(incremental["changed"], 1);

    // A caller that asked for a delta and got a rebuild must be able to see
    // that from the report alone, which is what separates the two spellings.
    git(temp.path(), &["commit", "-qam", "second"]);
    let fallback = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(fallback["mode"], "fallback-full");
    assert_eq!(fallback["fallback_reason"], "head-changed");

    let requested = json(&run(temp.path(), &["refresh", "--json", "--full"]));
    assert_eq!(requested["mode"], "full");
    assert_eq!(requested["fallback_reason"], Value::Null);
}

#[test]
fn map_is_refresh_full_under_another_name() {
    let temp = repo();

    let mapped = json(&run(temp.path(), &["map", "--json"]));

    assert_eq!(mapped["command"], "map", "the report names the command run");
    assert_eq!(mapped["mode"], "full", "map never consults a delta");
    assert_eq!(mapped["fallback_reason"], Value::Null);
}

#[test]
fn a_second_refresh_costs_nothing_and_says_so() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let value = json(&run(temp.path(), &["refresh", "--json"]));

    assert_eq!(value["outcome"], "unchanged");
    assert_eq!(value["reparsed"], 0);
    assert_eq!(value["content_reads"], 0);
    assert_eq!(value["snapshot_updated"], false);
}

/// A thread count of zero is a usage error, and gets the usage error's code.
///
/// Rejected by the parser rather than by the library, so it cannot be confused
/// with the repository being at fault — and so `2` keeps the one meaning the
/// contract gives it.
#[test]
fn a_rejected_thread_count_is_a_usage_error() {
    let temp = repo();

    let output = run(temp.path(), &["refresh", "--threads", "0"]);

    assert_eq!(code(&output), 2, "a bad argument is a usage error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--threads"), "got {stderr:?}");
}

/// An operational failure is a failure, and says so in one line.
#[test]
fn a_root_that_does_not_exist_is_an_operational_failure() {
    let temp = repo();

    let output = run(temp.path(), &["refresh", "--root", "no/such/place"]);

    assert_eq!(code(&output), 1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("rr: refresh: "), "got {stderr:?}");
    assert_eq!(stderr.lines().count(), 1, "one failure is one line");
}

#[test]
fn every_json_report_is_exactly_one_line() {
    let temp = repo();
    run(temp.path(), &["map"]);

    for args in [
        vec!["status", "--json"],
        vec!["refresh", "--json"],
        vec!["map", "--json"],
    ] {
        let text = stdout(&run(temp.path(), &args));
        assert!(text.ends_with('\n'), "{args:?} did not terminate its line");
        assert_eq!(
            text.lines().count(),
            1,
            "{args:?} emitted more than one object"
        );
    }
}

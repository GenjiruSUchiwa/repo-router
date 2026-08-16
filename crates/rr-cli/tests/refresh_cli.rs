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

use common::{code, commit_all, empty_repo, json, run, stderr, stdout, write};

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

/// Committing the maps keeps the next refresh incremental.
///
/// The two commits are compared directly, and generated maps are not indexed,
/// so the delta is empty. Metadata is republished; nothing is reparsed.
#[test]
fn committing_the_generated_maps_keeps_the_next_refresh_incremental() {
    let temp = repo();
    run(temp.path(), &["map"]);
    commit_all(temp.path(), "maps");

    let status = json(&run(temp.path(), &["status", "--json"]));
    assert_eq!(status["git"], "clean");
    assert_eq!(status["snapshot"], "fresh");

    let refreshed = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(refreshed["mode"], "incremental");
    assert_eq!(refreshed["fallback_reason"], Value::Null);
    assert_eq!(refreshed["reparsed"], 0);
    assert_eq!(refreshed["cached"], 0);

    let after = json(&run(temp.path(), &["status", "--json"]));
    assert_eq!(after["snapshot"], "fresh");
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

    write(temp.path(), ".gitignore", "target\n");
    commit_all(temp.path(), "rules");
    let fallback = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(fallback["mode"], "fallback-full");
    assert_eq!(fallback["fallback_reason"], "discovery-rules-changed");

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
        vec!["refresh", "--json", "--verbose"],
        vec!["map", "--json", "--verbose"],
    ] {
        let text = stdout(&run(temp.path(), &args));
        assert!(text.ends_with('\n'), "{args:?} did not terminate its line");
        assert_eq!(
            text.lines().count(),
            1,
            "{args:?} emitted more than one object"
        );
    }

    obstruct(&temp);
    let refused = stdout(&run(temp.path(), &["map", "--json"]));
    assert!(
        refused.ends_with('\n'),
        "a refusal did not terminate a line"
    );
    assert_eq!(refused.lines().count(), 1, "a refusal is one object too");
}

/// Leaves a file rr did not write at a reserved path.
fn obstruct(temp: &TempDir) {
    write(temp.path(), "MAP.md", "hand-written, not rr's\n");
}

#[test]
fn refresh_json_reports_the_text_artifacts_it_wrote() {
    let temp = repo();

    let mapped = json(&run(temp.path(), &["map", "--json"]));
    assert_eq!(mapped["schema_version"], 4);
    assert_eq!(mapped["outcome"], "updated");
    assert!(
        mapped["text"]["written"].as_u64().unwrap_or_default() > 0,
        "a first map writes committed artifacts: {mapped}"
    );
    assert_eq!(mapped["text"]["symbols"], "written");
    assert_eq!(mapped["text"]["removed"], 0);
    assert!(mapped["text"]["conflicts"]
        .as_array()
        .is_some_and(Vec::is_empty));

    std::fs::remove_file(temp.path().join("MAP.md")).expect("failed to drop the root map");
    let repaired = json(&run(temp.path(), &["refresh", "--json"]));
    assert_eq!(
        repaired["snapshot_updated"], false,
        "the snapshot was already current: {repaired}"
    );
    assert_eq!(
        repaired["outcome"], "updated",
        "rewriting the root map is a change: {repaired}"
    );
    assert!(repaired["text"]["written"].as_u64().unwrap_or_default() > 0);
}

#[test]
fn a_conflict_is_parseable_and_still_exits_one() {
    let temp = repo();
    run(temp.path(), &["map"]);
    obstruct(&temp);

    let output = run(temp.path(), &["map", "--json"]);

    assert_eq!(code(&output), 1, "a refusal is a failure");
    let value = json(&output);
    assert_eq!(value["outcome"], "refused");
    assert_eq!(value["snapshot_updated"], false);
    assert_eq!(value["text"]["written"], 0);
    let conflicts = value["text"]["conflicts"]
        .as_array()
        .expect("conflicts is an array");
    let first = conflicts.first().expect("at least one conflict");
    assert_eq!(first["path"], "MAP.md");
    assert_eq!(first["reason"], "not-owned");
    assert!(
        first["message"].as_str().is_some_and(|m| !m.is_empty()),
        "the sentence a human reads is in the object too: {value}"
    );
}

#[test]
fn a_conflict_still_explains_itself_to_a_human() {
    let temp = repo();
    run(temp.path(), &["map"]);
    obstruct(&temp);

    let output = run(temp.path(), &["map"]);

    assert_eq!(code(&output), 1);
    let stderr = stderr(&output);
    assert!(
        stderr.contains("text artifacts conflict with the repository:"),
        "got {stderr:?}"
    );
    assert!(stderr.contains("MAP.md"), "got {stderr:?}");
    assert!(stderr.contains("nothing was written"), "got {stderr:?}");
    assert!(stdout(&output).is_empty(), "a refusal reports no summary");
}

#[test]
fn the_human_summary_line_is_unchanged_by_this_issue() {
    let temp = repo();

    let mapped = stdout(&run(temp.path(), &["map"]));
    let line = mapped.lines().next().unwrap_or_default();
    assert!(line.starts_with("rr map — updated,"), "got {line:?}");
    assert!(
        line.contains("; text: ")
            && line.contains(" written, ")
            && line.contains(" unchanged, ")
            && line.contains(" removed, SYMBOLS.md written, ")
            && line.ends_with(" purpose pending"),
        "got {line:?}"
    );

    let again = stdout(&run(temp.path(), &["refresh"]));
    let line = again.lines().next().unwrap_or_default();
    assert!(line.starts_with("rr refresh — unchanged,"), "got {line:?}");
    assert!(line.contains("SYMBOLS.md unchanged"), "got {line:?}");
}

#[test]
fn verbose_output_is_the_same_facts_on_both_surfaces() {
    let temp = repo();

    let text = stdout(&run(temp.path(), &["map", "--verbose"]));
    assert!(text.contains("  text write   MAP.md"), "got {text}");
    assert!(text.contains("  snapshot: republished"), "got {text}");

    std::fs::remove_file(temp.path().join("MAP.md")).expect("failed to drop the root map");
    let value = json(&run(temp.path(), &["map", "--json", "--verbose"]));
    let written = value["text"]["written_paths"]
        .as_array()
        .expect("verbose JSON carries the paths");
    assert!(written.iter().any(|path| path == "MAP.md"), "got {value}");

    let quiet = json(&run(temp.path(), &["refresh", "--json"]));
    assert!(
        quiet["text"]["written_paths"].is_null(),
        "the summary object stays a summary: {quiet}"
    );
}

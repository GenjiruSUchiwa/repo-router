//! The published contract of `rr init`.
//!
//! Four destinations, four independent outcomes, and an exit code that is `0`
//! only when every one of them is installed and current. A refusal names the
//! file and leaves it alone; the other three still land. A second run writes
//! nothing, proved by comparing bytes.

mod common;

use std::path::Path;

use serde_json::Value;
use tempfile::TempDir;

use common::*;
use rr_core::agent::{
    is_rr_written_skill, skill_document, AGENT_FORMAT_VERSION, CONTRACT_BEGIN_MARKER,
    CONTRACT_BLOCK, CONTRACT_END_MARKER, SKILL_PATH,
};
use rr_core::text::{Digest, IGNORE_BEGIN_MARKER, IGNORE_END_MARKER, IGNORE_PATH};

const RESTART_NOTE: &str = ".claude/skills/ is new; restart Claude Code so it picks up the skill.";

const FOUR_TARGETS: [&str; 4] = [IGNORE_PATH, "AGENTS.md", "CLAUDE.md", SKILL_PATH];

fn empty_dir() -> TempDir {
    TempDir::new().expect("failed to create temp dir")
}

fn file_bytes(root: &Path, path: &str) -> Vec<u8> {
    std::fs::read(root.join(path)).unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn outcome_of(report: &Value, path: &str) -> String {
    report["targets"]
        .as_array()
        .expect("targets is an array")
        .iter()
        .find(|target| target["path"] == path)
        .unwrap_or_else(|| panic!("missing target {path}"))["outcome"]
        .as_str()
        .expect("outcome is a string")
        .to_owned()
}

fn reason_of(report: &Value, path: &str) -> String {
    report["targets"]
        .as_array()
        .expect("targets is an array")
        .iter()
        .find(|target| target["path"] == path)
        .unwrap_or_else(|| panic!("missing target {path}"))["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("target {path} carries no reason"))
        .to_owned()
}

fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("failed to list directory")
        .map(|entry| {
            entry
                .expect("failed to read directory entry")
                .file_name()
                .into_string()
                .expect("directory entry is not UTF-8")
        })
        .collect();
    names.sort();
    names
}

/// A stamped `SKILL.md` rr would accept as its own, but whose body is older.
///
/// The stamp has to verify: a hash that does not match the bytes is somebody
/// else's file, and that path is refused rather than upgraded.
fn older_stamped_skill() -> String {
    let current = skill_document();
    let version_line = format!("format: {AGENT_FORMAT_VERSION}");
    assert!(
        current.contains(&version_line),
        "skill_document must name AGENT_FORMAT_VERSION so the older file can differ"
    );
    let restamped = current.replace(&version_line, "format: 0");
    let unstamped = unstamp(&restamped).expect("skill_document carries a stamp line");
    let digest = Digest::of_bytes(unstamped.as_bytes());
    unstamped.replacen(
        "    generated_hash: \"\"",
        &format!("    generated_hash: \"{}\"", digest.to_text()),
        1,
    )
}

fn unstamp(text: &str) -> Option<String> {
    const PREFIX: &str = "    generated_hash: \"";
    let mut out = String::with_capacity(text.len());
    let mut found = false;
    for line in text.lines() {
        if line.starts_with(PREFIX) && line.ends_with('"') {
            out.push_str("    generated_hash: \"\"\n");
            found = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    found.then_some(out)
}

#[test]
fn init_on_an_empty_directory_creates_all_four_targets() {
    let temp = empty_dir();

    let output = run(temp.path(), &["init"]);

    assert_eq!(code(&output), 0);
    let ignore = read(temp.path(), IGNORE_PATH);
    assert!(
        ignore.contains(IGNORE_BEGIN_MARKER),
        "missing ignore begin marker"
    );
    assert!(
        ignore.contains(IGNORE_END_MARKER),
        "missing ignore end marker"
    );
    assert_eq!(read(temp.path(), "AGENTS.md"), CONTRACT_BLOCK);
    assert_eq!(read(temp.path(), "CLAUDE.md"), CONTRACT_BLOCK);
    let skill = read(temp.path(), SKILL_PATH);
    assert!(
        skill.contains("generated_hash: \"blake3:"),
        "skill is missing its blake3 stamp"
    );
    assert!(
        is_rr_written_skill(&skill),
        "installed skill must verify as rr-written"
    );
}

/// `rr init` stamps `.rr/.gitignore` itself on the way in, so the managed block
/// finds a file that already exists. Reporting that as `updated` would tell a
/// user their file was changed when rr had just made it.
#[test]
fn a_fresh_ignore_file_is_reported_as_created() {
    let temp = empty_dir();

    let output = run(temp.path(), &["init", "--json"]);

    assert_eq!(code(&output), 0);
    let report = json(&output);
    for path in FOUR_TARGETS {
        assert_eq!(
            outcome_of(&report, path),
            "created",
            "{path} was not created"
        );
    }
}

/// The two marker refusals are different repairs, so they must be different
/// messages. Telling a user with two regions to "fix or delete the region"
/// sends them looking for a problem that is not there.
#[test]
fn duplicated_and_unpaired_markers_report_different_reasons() {
    let temp = empty_dir();
    write(
        temp.path(),
        "AGENTS.md",
        &format!(
            "{CONTRACT_BEGIN_MARKER}\none\n{CONTRACT_END_MARKER}\n\
             {CONTRACT_BEGIN_MARKER}\ntwo\n{CONTRACT_END_MARKER}\n"
        ),
    );
    write(
        temp.path(),
        "CLAUDE.md",
        &format!("{CONTRACT_BEGIN_MARKER}\n"),
    );

    let output = run(temp.path(), &["init", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(
        reason_of(&report, "AGENTS.md").contains("more than once"),
        "a duplicated region must say so: {}",
        reason_of(&report, "AGENTS.md")
    );
    assert!(
        reason_of(&report, "CLAUDE.md").contains("unpaired or inverted"),
        "an unclosed region must say so: {}",
        reason_of(&report, "CLAUDE.md")
    );
}

/// A run that installed nothing has nothing to commit.
#[test]
fn a_run_that_refuses_everything_does_not_advise_committing() {
    let temp = empty_dir();
    let unpaired = format!("{CONTRACT_BEGIN_MARKER}\n");
    write(temp.path(), IGNORE_PATH, IGNORE_BEGIN_MARKER);
    write(temp.path(), "AGENTS.md", &unpaired);
    write(temp.path(), "CLAUDE.md", &unpaired);
    write(temp.path(), SKILL_PATH, "# mine\n");

    let output = run(temp.path(), &["init"]);

    assert_eq!(code(&output), 1);
    assert!(
        !stdout(&output).contains("commit them"),
        "nothing was installed, so nothing is there to commit: {}",
        stdout(&output)
    );
}

/// Idempotency is proved by bytes, not by mtime: the second run neither
/// rewrites identical content nor reports anything but `unchanged`.
#[test]
fn a_second_init_writes_nothing() {
    let temp = empty_dir();
    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let before: Vec<Vec<u8>> = FOUR_TARGETS
        .iter()
        .map(|path| file_bytes(temp.path(), path))
        .collect();

    let second = run(temp.path(), &["init", "--json"]);
    assert_eq!(code(&second), 0);

    let after: Vec<Vec<u8>> = FOUR_TARGETS
        .iter()
        .map(|path| file_bytes(temp.path(), path))
        .collect();
    assert_eq!(before, after, "a second init must not change any target");

    let report = json(&second);
    for path in FOUR_TARGETS {
        assert_eq!(
            outcome_of(&report, path),
            "unchanged",
            "{path} was not unchanged"
        );
    }
}

#[test]
fn an_existing_agents_file_keeps_its_own_content() {
    let temp = empty_dir();
    let own = "one\ntwo\nthree\nfour\nfive\n";
    write(temp.path(), "AGENTS.md", own);

    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let after = read(temp.path(), "AGENTS.md");
    assert!(
        after.starts_with(own),
        "the five existing lines must survive byte-for-byte"
    );
    assert_eq!(
        &after[own.len()..],
        format!("\n{CONTRACT_BLOCK}"),
        "the contract is appended after one blank line"
    );
}

/// `CLAUDE.md` and `claude.md` are one file on a folding filesystem and two
/// on Linux. Adopting the existing spelling is what stops a second copy.
#[test]
fn an_existing_claude_file_is_found_case_insensitively() {
    let temp = empty_dir();
    write(temp.path(), "claude.md", "already here\n");

    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let names = entry_names(temp.path());
    assert!(
        names.iter().any(|name| name == "claude.md"),
        "the existing spelling must be kept: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "CLAUDE.md"),
        "a second CLAUDE.md must not be created: {names:?}"
    );
    let claude = read(temp.path(), "claude.md");
    assert!(
        claude.starts_with("already here\n"),
        "existing prose is preserved"
    );
    assert!(
        claude.contains(CONTRACT_BEGIN_MARKER) && claude.contains(CONTRACT_END_MARKER),
        "claude.md must gain the contract block"
    );
}

/// Duplicated markers usually mean a merge kept both sides. Picking one would
/// silently discard somebody's edit, so the file is left alone.
#[test]
fn a_duplicated_contract_region_is_refused_and_the_file_is_untouched() {
    let temp = empty_dir();
    let planted = format!(
        "{CONTRACT_BEGIN_MARKER}\nfirst\n{CONTRACT_END_MARKER}\n\
         {CONTRACT_BEGIN_MARKER}\nsecond\n{CONTRACT_END_MARKER}\n"
    );
    write(temp.path(), "AGENTS.md", &planted);

    let output = run(temp.path(), &["init", "--json"]);
    assert_eq!(code(&output), 1);
    assert_eq!(read(temp.path(), "AGENTS.md"), planted);

    let report = json(&output);
    assert_eq!(outcome_of(&report, "AGENTS.md"), "refused");

    let ignore = read(temp.path(), IGNORE_PATH);
    assert!(ignore.contains(IGNORE_BEGIN_MARKER));
    assert_eq!(read(temp.path(), "CLAUDE.md"), CONTRACT_BLOCK);
    assert!(is_rr_written_skill(&read(temp.path(), SKILL_PATH)));
}

/// A user is entitled to keep their own skill at this path. The other three
/// targets still install.
#[test]
fn a_hand_written_skill_is_refused_and_preserved() {
    let temp = empty_dir();
    let hand = "# my skill\n\nnot written by rr\n";
    write(temp.path(), SKILL_PATH, hand);

    let output = run(temp.path(), &["init", "--json"]);
    assert_eq!(code(&output), 1);
    assert_eq!(read(temp.path(), SKILL_PATH), hand);

    let report = json(&output);
    assert_eq!(outcome_of(&report, SKILL_PATH), "refused");
    assert_eq!(
        outcome_of(&report, "AGENTS.md"),
        "created",
        "a refused skill must not stop AGENTS.md from being created"
    );
    assert_eq!(read(temp.path(), "AGENTS.md"), CONTRACT_BLOCK);
}

#[test]
fn an_rr_written_skill_is_upgraded_in_place() {
    let temp = empty_dir();
    let older = older_stamped_skill();
    assert!(
        is_rr_written_skill(&older),
        "the fixture must verify or the upgrade path is the refusal path"
    );
    assert_ne!(older, skill_document());
    write(temp.path(), SKILL_PATH, &older);

    let output = run(temp.path(), &["init", "--json"]);
    assert_eq!(code(&output), 0);
    assert_eq!(outcome_of(&json(&output), SKILL_PATH), "updated");
    assert_eq!(read(temp.path(), SKILL_PATH), skill_document());
}

#[test]
fn a_stale_contract_region_is_replaced_not_appended() {
    let temp = empty_dir();
    write(
        temp.path(),
        "AGENTS.md",
        &format!("{CONTRACT_BEGIN_MARKER}\nold contract\n{CONTRACT_END_MARKER}\n"),
    );

    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let after = read(temp.path(), "AGENTS.md");
    assert_eq!(
        after.matches(CONTRACT_BEGIN_MARKER).count(),
        1,
        "a stale region is replaced, not appended"
    );
    assert_eq!(after, CONTRACT_BLOCK);
    assert!(
        !after.contains("old contract"),
        "the stale body must not survive"
    );
}

#[test]
fn the_state_directory_is_private_after_init() {
    let temp = empty_dir();

    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let ignore = read(temp.path(), IGNORE_PATH);
    assert!(
        ignore.contains(IGNORE_BEGIN_MARKER) && ignore.contains(IGNORE_END_MARKER),
        ".rr/.gitignore must carry the managed block"
    );
}

#[test]
fn init_reports_json_that_parses() {
    let temp = empty_dir();

    let output = run(temp.path(), &["init", "--json"]);
    assert_eq!(code(&output), 0);

    let text = stdout(&output);
    assert!(text.ends_with('\n'), "JSON must terminate its line");
    assert_eq!(text.lines().count(), 1, "stdout is one JSON object");

    let report = json(&output);
    assert_eq!(report["v"], 1);
    assert_eq!(
        report["targets"]
            .as_array()
            .expect("targets is an array")
            .len(),
        4
    );
}

/// Amendment C: a newly created `.claude/skills/` is invisible until Claude
/// Code is restarted, so the run that creates it has to say so.
#[test]
fn the_first_init_says_to_restart_claude_code() {
    let temp = empty_dir();

    let output = run(temp.path(), &["init"]);

    assert_eq!(code(&output), 0);
    assert!(
        stdout(&output).contains(RESTART_NOTE),
        "first init must mention the Claude Code restart: {}",
        stdout(&output)
    );
}

/// A notice printed every run is a notice nobody reads.
#[test]
fn a_second_init_does_not_say_to_restart() {
    let temp = empty_dir();
    assert_eq!(code(&run(temp.path(), &["init"])), 0);

    let second = run(temp.path(), &["init"]);
    assert_eq!(code(&second), 0);
    assert!(
        !stdout(&second).contains(RESTART_NOTE),
        "a later init must not repeat the restart note: {}",
        stdout(&second)
    );
}

/// `rr init` reads no Git state, so a directory that is not a repository is
/// a valid place to install the contract.
#[test]
fn init_is_safe_in_a_directory_that_is_not_a_repository() {
    let temp = empty_dir();
    assert!(!temp.path().join(".git").exists());

    let output = run(temp.path(), &["init"]);

    assert_eq!(code(&output), 0);
    assert!(!temp.path().join(".git").exists());
}

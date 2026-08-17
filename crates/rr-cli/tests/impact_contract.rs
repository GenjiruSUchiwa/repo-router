//! `rr impact` as an agent and a CI script meet it.
//!
//! The unit tests in `rr-core` pin what an edge *is*, row by row of the truth
//! table. These pin the contract around it: the exit code a script branches on,
//! the two renderings agreeing about every number, the truncation that bounds
//! what a human reads without bounding what was computed, and byte-identical
//! output across a hundred runs — because a report that is not byte-identical
//! cannot be reviewed in a diff.
//!
//! Every fixture is forged from the outside, the way a change arrives in real
//! life: an edit in the working tree, a commit, a filter that rewrites bytes
//! between two reads. Nothing here reaches into a private API to manufacture a
//! state a repository could not get into on its own.

mod common;

use std::path::Path;

use serde_json::Value;
use tempfile::TempDir;

use common::{code, commit_all, empty_repo, git, json, read, run, stderr, stdout, write};

/// How many times a determinism test runs one command.
///
/// A hundred, and in both modes, because the failure it guards against is not a
/// wrong value but an *unstable* one: a hash-map walk, an unsorted vector or a
/// clock reaches the output on some runs and not others.
const RUNS: usize = 100;

/// The workspace fixture, copied into a repository of its own.
///
/// `fixtures/rust-basic` is a checked-in corpus rather than a repository, so it
/// is copied rather than used in place: these tests commit to it, edit it and
/// race it, and none of that may touch the tree the test is running from.
fn rust_basic() -> TempDir {
    let temp = empty_repo();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-basic");
    copy_into(&source, temp.path());
    commit_all(temp.path(), "seed");
    assert_eq!(
        code(&run(temp.path(), &["map"])),
        0,
        "the fixture must index"
    );
    temp
}

/// Copies a directory tree, so a fixture can be committed to without being
/// modified.
fn copy_into(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).expect("failed to read a fixture directory") {
        let entry = entry.expect("failed to read a fixture entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&target).expect("failed to create a fixture directory");
            copy_into(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("failed to copy a fixture file");
        }
    }
}

/// Rewrites one fixture file, asserting the text being replaced was there.
fn edit(root: &Path, path: &str, from: &str, to: &str) {
    let before = read(root, path);
    let after = before.replace(from, to);
    assert_ne!(after, before, "the text to replace was not in {path}");
    write(root, path, &after);
}

/// The `verify_token` edit every acceptance assertion is about.
fn edit_verify_token(root: &Path) {
    edit(
        root,
        "src/auth/token.rs",
        "claims.exp > now()",
        "claims.exp > now() && !claims.sub.is_empty()",
    );
}

/// The anchor of every entry of one JSON array.
///
/// The anchor and not the qualified name, because the anchor is what a caller
/// navigates by and what identifies a node across two endpoints; a qualified
/// name is a rendering of whatever the extractor could see.
fn anchors(report: &Value, section: &str) -> Vec<String> {
    report[section]
        .as_array()
        .unwrap_or_else(|| panic!("{section} must be an array: {report}"))
        .iter()
        .map(|node| node["anchor"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The one test-file entry `tests` holds for `path`.
fn test_entry(report: &Value, path: &str) -> Value {
    report["tests"]
        .as_array()
        .expect("tests must be an array")
        .iter()
        .find(|test| test["path"] == path)
        .unwrap_or_else(|| panic!("{path} is not in tests: {}", report["tests"]))
        .clone()
}

/// The `resolution:` line of one text report.
fn resolution_line(text: &str) -> String {
    text.lines()
        .find(|line| line.starts_with("resolution: "))
        .expect("every report states its counters")
        .to_owned()
}

/// The acceptance case: an edited definition, its caller, and its callee.
///
/// The related test appears with reason `lexical-name` and not
/// `resolved-reference`, and the assertion below proves why rather than
/// asserting it on trust: `tests/token_test.rs` never names `verify_token`, so
/// no resolved edge to it can exist and the only evidence left is a shared name
/// term. A report that claimed an exact edge here would be claiming one the
/// index does not hold.
#[test]
fn verify_token_edit_shows_main_affected_and_users_dependency() {
    let repo = rust_basic();
    assert!(
        !read(repo.path(), "tests/token_test.rs").contains("verify_token"),
        "the fixture test must not name the changed definition, or the reason below is not lexical"
    );
    edit_verify_token(repo.path());

    let output = run(repo.path(), &["impact", "--json"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let report = json(&output);

    assert!(
        anchors(&report, "affected").contains(&String::from("src/main.rs#main")),
        "the caller of the edited definition is missing: {}",
        report["affected"]
    );
    assert!(
        anchors(&report, "dependencies")
            .iter()
            .any(|anchor| anchor.starts_with("src/db/users.rs#")),
        "the callee is missing from dependencies: {}",
        report["dependencies"]
    );

    let test = test_entry(&report, "tests/token_test.rs");
    assert_eq!(test["reasons"], serde_json::json!(["lexical-name"]));
    assert_eq!(test["confidence"], "probable");
}

#[test]
fn no_changes_exits_zero_with_empty_sections() {
    let repo = rust_basic();

    let output = run(repo.path(), &["impact"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    for section in [
        "changed files",
        "changed definitions",
        "direct edges",
        "affected",
        "dependencies",
        "cycles",
        "tests",
        "diagnostics",
        "unfollowed imports",
    ] {
        assert!(
            text.contains(&format!("{section}: none")),
            "{section} was not printed as empty: {text}"
        );
    }
}

/// `--head` names one endpoint, and the other is `--base`'s default.
///
/// The requirement is discharged by the default rather than by a refusal: the
/// contract says `--head` requires `--base`, and `--base` is always present
/// because it defaults to `HEAD`. What must never happen is a comparison against
/// an unnamed endpoint, so this pins the base the report actually used.
#[test]
fn head_requires_base() {
    let repo = rust_basic();
    edit_verify_token(repo.path());
    commit_all(repo.path(), "edit verify_token");

    let output = run(repo.path(), &["impact", "--json", "--head", "HEAD"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let report = json(&output);
    assert_eq!(report["base"]["spec"], "HEAD");
    assert_eq!(report["target"]["kind"], "tree");
}

#[test]
fn head_and_worktree_are_mutually_exclusive() {
    let repo = rust_basic();

    let output = run(repo.path(), &["impact", "--head", "HEAD", "--worktree"]);

    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot be used with"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "a refused run printed a report");
}

#[test]
fn depth_nine_is_a_usage_error() {
    let repo = rust_basic();

    let output = run(repo.path(), &["impact", "--depth", "9"]);

    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains("0..=8"), "{}", stderr(&output));
}

#[test]
fn limit_zero_is_a_usage_error() {
    let repo = rust_basic();

    let output = run(repo.path(), &["impact", "--limit", "0"]);

    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert!(stderr(&output).contains("1..=1000"), "{}", stderr(&output));
}

/// A directory with no repository in it is `2`, and says so once.
///
/// One line, because a caller reading stderr line by line would read a second
/// line as a second failure — and `2` rather than `1`, because nothing about the
/// repository was ever evaluated.
#[test]
fn non_git_root_exits_two_with_one_stderr_line() {
    let plain = TempDir::new().expect("failed to create temp dir");
    write(plain.path(), "src/lib.rs", "pub fn one() {}\n");

    let output = run(plain.path(), &["impact"]);

    assert_eq!(code(&output), 2);
    let message = stderr(&output);
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(message.starts_with("rr: impact: "), "{message}");
    assert!(stdout(&output).is_empty(), "a refused run printed a report");
}

#[test]
fn unparsable_revision_exits_two() {
    let repo = rust_basic();

    let output = run(repo.path(), &["impact", "--base", "no/such/revision"]);

    assert_eq!(code(&output), 2);
    let message = stderr(&output);
    assert_eq!(message.lines().count(), 1, "{message}");
    assert!(message.contains("unresolvable revision"), "{message}");
}

/// A file whose bytes moved between the observation and the read.
///
/// Forged with a clean filter that answers differently every time it is asked,
/// which is the one way to make the race happen on purpose. The report must
/// still be printed: `1` means *this is the report, and this part of it is
/// missing*, and a caller that got no report could not tell which part.
#[test]
#[cfg(unix)]
fn partial_report_exits_one_and_still_prints() {
    let repo = empty_repo();
    write(
        repo.path(),
        "src/moving.rs",
        "pub fn one() {}\npub fn two() {}\n",
    );
    commit_all(repo.path(), "seed");

    let tick = repo.path().join("tick");
    std::fs::write(&tick, b"").expect("failed to seed the tick file");
    let driver = format!(
        "cat >/dev/null; wc -c < {tick}; printf x >> {tick}",
        tick = tick.display()
    );
    git(repo.path(), &["config", "filter.tick.clean", &driver]);
    write(repo.path(), ".gitattributes", "*.rs filter=tick\n");
    write(
        repo.path(),
        "src/moving.rs",
        "pub fn one() {}\npub fn two(now: u8) {}\n",
    );

    let output = run(repo.path(), &["impact"]);

    assert_eq!(code(&output), 1, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.starts_with("impact: partial"), "{text}");
    assert!(text.contains("IMPACT_WORKTREE_RACED"), "{text}");
    assert!(
        text.contains("src/moving.rs"),
        "the report hid the path it could not describe: {text}"
    );
}

/// `--limit` bounds the human report and nothing else.
///
/// Three things are checked together because the bug is always one of the three:
/// the text says `shown 1/N` so the total survives truncation, the counters are
/// the same numbers a full render prints, and the JSON object is complete —
/// a truncated object is one a consumer cannot tell from a small one.
#[test]
fn limit_truncates_rendering_not_counters() {
    let repo = rust_basic();
    edit_verify_token(repo.path());

    let full = run(repo.path(), &["impact"]);
    let bounded = run(repo.path(), &["impact", "--limit", "1"]);
    let object = json(&run(repo.path(), &["impact", "--json", "--limit", "1"]));

    assert_eq!(code(&bounded), 0, "{}", stderr(&bounded));
    let text = stdout(&bounded);
    assert!(text.contains("affected (shown 1/3):"), "{text}");
    assert_eq!(
        resolution_line(&text),
        resolution_line(&stdout(&full)),
        "truncating the report changed a counter"
    );
    assert_eq!(
        object["affected"]
            .as_array()
            .expect("affected must be an array")
            .len(),
        3,
        "the JSON object was truncated: {}",
        object["affected"]
    );
}

/// An import this index cannot follow is listed, and the note states the count.
///
/// Python resolves imports by name rather than by path, so no import edge in
/// `src/app.py` can exist. An empty `affected` there reads as "nothing depends
/// on this", which is the one thing it does not mean, and the note is what
/// stops that reading.
#[test]
fn unfollowed_imports_are_printed_with_a_note() {
    let repo = rust_basic();
    write(
        repo.path(),
        "src/app.py",
        "from auth import verify_token\n\n\ndef check(token):\n    return verify_token(token)\n",
    );
    commit_all(repo.path(), "add a python caller");
    edit(
        repo.path(),
        "src/app.py",
        "def check(token)",
        "def check(t)",
    );

    let output = run(repo.path(), &["impact"]);
    let report = json(&run(repo.path(), &["impact", "--json"]));

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("unfollowed imports (1):"), "{text}");
    assert!(
        text.contains(
            "note: 1 import(s) in changed files cannot be followed (this language resolves imports by name, not path)"
        ),
        "{text}"
    );
    let import = &report["unfollowed_imports"][0];
    assert_eq!(import["path"], "src/app.py");
    assert_eq!(import["specifier"], "auth");
    assert_eq!(import["name"], "verify_token");
    assert_eq!(import["why"], "unresolved-by-design");
    assert_eq!(report["resolution"]["unresolved_imports"], 1);
}

/// The two renderings are one value, so every number in one is in the other.
///
/// Both directions of the same promise: each counter's name and value appear in
/// the text line, and each section's header states the length of the array the
/// JSON object publishes. Rendering them apart is exactly how the two forms
/// drift.
#[test]
fn text_and_json_agree_on_every_count() {
    let repo = rust_basic();
    edit_verify_token(repo.path());

    let text = stdout(&run(repo.path(), &["impact"]));
    let report = json(&run(repo.path(), &["impact", "--json"]));

    let counters = report["resolution"]
        .as_object()
        .expect("resolution must be an object");
    for (key, value) in counters {
        let label = key.replace('_', " ");
        assert!(
            resolution_line(&text).contains(&format!("{label} {value}")),
            "the text report does not state {label} {value}: {text}"
        );
    }

    for (section, label) in [
        ("changed_files", "changed files"),
        ("changed_definitions", "changed definitions"),
        ("direct_edges", "direct edges"),
        ("affected", "affected"),
        ("dependencies", "dependencies"),
        ("cycles", "cycles"),
        ("tests", "tests"),
        ("unfollowed_imports", "unfollowed imports"),
    ] {
        let total = report[section]
            .as_array()
            .unwrap_or_else(|| panic!("{section} must be an array"))
            .len();
        let expected = if total == 0 {
            format!("{label}: none")
        } else {
            format!("{label} ({total}):")
        };
        assert!(
            text.contains(&expected),
            "the text report does not head {section} with {expected:?}: {text}"
        );
    }
}

/// A hundred runs of one comparison, in both modes, are one byte string.
///
/// The report is meant to be diffable in a pull request, which a timestamp, an
/// elapsed time, an absolute path or a hash-map walk would make impossible. The
/// fixture is small on purpose: what this test measures is stability, and a
/// hundred runs of a large corpus would measure patience.
#[test]
fn one_hundred_runs_are_byte_identical() {
    let repo = empty_repo();
    write(
        repo.path(),
        "src/auth/token.rs",
        "pub fn verify_token(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    );
    write(
        repo.path(),
        "src/main.rs",
        "mod auth;\nuse auth::token::verify_token;\n\nfn main() {\n    let _ = verify_token(\"t\");\n}\n",
    );
    commit_all(repo.path(), "seed");
    edit(
        repo.path(),
        "src/auth/token.rs",
        "!token.is_empty()",
        "!token.is_empty() && token.len() > 3",
    );

    for mode in [vec!["impact"], vec!["impact", "--json"]] {
        let first = stdout(&run(repo.path(), &mode));
        assert!(!first.is_empty(), "rr {mode:?} printed nothing");
        for attempt in 1..RUNS {
            assert_eq!(
                stdout(&run(repo.path(), &mode)),
                first,
                "run {attempt} of rr {mode:?} differed from the first"
            );
        }
    }
}

/// A consumer that stopped reading kills the run, and says nothing about it.
///
/// The disposition is restored once, in `main`; this pins that `rr impact`
/// inherits it rather than growing a second answer. `141` and a diagnostic would
/// both be wrong: the caller asked for the report and then went away, which is
/// not a failure of the command.
#[test]
#[cfg(unix)]
fn sigpipe_on_a_closed_stdout_is_silent() {
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    let repo = rust_basic();
    edit_verify_token(repo.path());

    let mut fds = [0 as libc::c_int; 2];
    assert_eq!(
        unsafe { libc::pipe(fds.as_mut_ptr()) },
        0,
        "pipe(2) failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { libc::close(fds[0]) };
    let closed = unsafe { Stdio::from_raw_fd(fds[1]) };

    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .args(["impact"])
        .current_dir(repo.path())
        .stdout(closed)
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn rr")
        .wait_with_output()
        .expect("failed to wait for rr");

    assert_eq!(
        output.status.signal(),
        Some(libc::SIGPIPE),
        "rr impact did not die of SIGPIPE (status {:?})",
        output.status
    );
    assert_eq!(
        output.status.code(),
        None,
        "rr impact returned an exit code, so it was never signalled"
    );
    assert!(
        output.stderr.is_empty(),
        "rr impact said {:?} on stderr",
        String::from_utf8_lossy(&output.stderr)
    );
}

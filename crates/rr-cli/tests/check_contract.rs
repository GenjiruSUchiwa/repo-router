//! `rr check` as a CI pipeline meets it.
//!
//! The unit tests in `rr-core` pin what each rule *is*. These pin the contract a
//! script depends on: the rule id it greps for, the exit code it branches on,
//! the precedence between two findings of different weight, and the promise that
//! running the command a second time still reports the same repository — because
//! the command changed nothing about it.
//!
//! Every fixture is forged from the outside, the way a broken repository arrives
//! in real life: a truncated file, an edited generated region, a path somebody
//! else's file is sitting on. Nothing here reaches into a private API to
//! manufacture a state the repository could not get into on its own.

mod common;

use std::fmt::Write as _;
use std::path::Path;

use rr_core::check::{conflict_diagnostic, EMITTED_RULES, RESERVED_RULES};
use rr_core::text::{ConflictReason, IGNORE_PATH, ROUTES_PATH, SYMBOLS_PATH};
use serde_json::Value;
use tempfile::TempDir;

use common::{code, commit_all, empty_repo, json, read, run, stdout, write};

const SNAPSHOT: &str = ".rr/local/snapshot.bin";
const MAX_QUALITY_REPORT_BYTES: u64 = 16 * 1024 * 1024;

/// A `manifest_digest` in the only spelling `Digest::parse` accepts.
const CORPUS: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

/// Three public symbols across two scopes, indexed and mapped.
///
/// Small on purpose: every diagnostic these tests assert is forged, so a fixture
/// large enough to raise one by accident would make a passing test unreadable.
/// Three and not two because the `RR03xx` tests need a question the ranker
/// answers *directly* — a route is only learned from a direct hit, and a corpus
/// of two names gives every question a field of candidates instead.
fn repo() -> TempDir {
    let temp = empty_repo();
    let root = temp.path();

    write(
        root,
        "src/auth/token.rs",
        "/// Verifies a bearer token against the signing key.\n\
         pub fn verify_token(token: &str) -> bool {\n    \
         !token.is_empty()\n\
         }\n",
    );
    write(
        root,
        "src/auth/keys.rs",
        "/// Rotates the signing key the verifier trusts.\n\
         pub fn rotate_signing_key() {}\n",
    );
    write(
        root,
        "src/store/entry.rs",
        "/// Encodes an entry into its wire representation.\n\
         pub fn serialize_entry() {}\n",
    );

    commit_all(root, "seed");
    assert_eq!(code(&run(root, &["map"])), 0, "the fixture must index");
    temp
}

/// Asks one question and asserts it was answered well enough to be learned.
///
/// A route is filed only for a direct hit, so a test that just ran the query and
/// ignored the answer would go on to assert things about an empty cache.
fn learn(root: &Path, question: &str) {
    let answer = json(&run(root, &["query", "--json", question]));
    assert_eq!(
        answer["result"], "direct",
        "the fixture must answer {question:?} directly: {answer}"
    );
}

/// The rule ids one report raises, in the order the report prints them.
fn rules(report: &Value) -> Vec<String> {
    report["diagnostics"]
        .as_array()
        .expect("diagnostics must be an array")
        .iter()
        .map(|diagnostic| diagnostic["rule_id"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// The one diagnostic carrying `rule_id`, or a panic naming what was found.
fn only(report: &Value, rule_id: &str) -> Value {
    let matching: Vec<&Value> = report["diagnostics"]
        .as_array()
        .expect("diagnostics must be an array")
        .iter()
        .filter(|diagnostic| diagnostic["rule_id"] == rule_id)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one {rule_id}, got {:?}",
        rules(report)
    );
    matching[0].clone()
}

/// Rewrites a file whose bytes a test wants to forge.
fn replace(root: &Path, path: &str, from: &str, to: &str) {
    let before = read(root, path);
    let after = before.replace(from, to);
    assert_ne!(
        after, before,
        "the fixture text to replace was not in {path}"
    );
    std::fs::write(root.join(path), after).expect("failed to rewrite file");
}

/// A quality report whose findings are spelled by the caller.
fn quality_report(dir: &Path, name: &str, findings: &str) -> String {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "{{\"schema_version\":1,\"manifest_digest\":\"{CORPUS}\",\"findings\":[{findings}]}}"
        ),
    )
    .expect("failed to write the quality report");
    path.display().to_string()
}

#[test]
fn clean_repository_exits_zero() {
    let temp = repo();
    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 0, "{}", stdout(&output));
    let report = json(&output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], "check");
    assert_eq!(report["status"], "ok");
    assert_eq!(rules(&report), Vec::<String>::new());
    assert!(
        report.get("quality").is_none(),
        "an absent --quality-report must omit the key rather than publish null"
    );
}

#[test]
fn missing_snapshot_is_fatal_and_exits_four() {
    let temp = repo();
    std::fs::remove_file(temp.path().join(SNAPSHOT)).expect("failed to remove the snapshot");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 4);
    let report = json(&output);
    assert_eq!(report["status"], "snapshot-missing");
    assert_eq!(
        only(&report, "RR0001_SNAPSHOT_MISSING")["severity"],
        "fatal"
    );
    assert_eq!(
        rules(&report).len(),
        1,
        "nothing else can be asked without a snapshot"
    );
}

#[test]
fn truncated_snapshot_is_rr0002_and_exits_three() {
    let temp = repo();
    let path = temp.path().join(SNAPSHOT);
    let bytes = std::fs::read(&path).expect("failed to read the snapshot");
    std::fs::write(&path, &bytes[..64]).expect("failed to truncate the snapshot");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    assert_eq!(report["status"], "errors");
    let diagnostic = only(&report, "RR0002_SNAPSHOT_CORRUPT");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["actual"], "length-mismatch");
}

/// The envelope's own version word is the half of the vintage check that can be
/// forged from outside, and it is validated before the checksum, so no checksum
/// has to be repaired. `BuildVersionMismatch` is read off the decoded payload
/// and would mean re-encoding a snapshot; it reports under this same rule id,
/// and `check.rs`'s unit test covers all ten `RebuildReason`s.
#[test]
fn snapshot_from_an_older_build_version_is_rr0003() {
    let temp = repo();
    let path = temp.path().join(SNAPSHOT);
    let mut bytes = std::fs::read(&path).expect("failed to read the snapshot");
    bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
    std::fs::write(&path, &bytes).expect("failed to rewrite the snapshot");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0003_SNAPSHOT_INCOMPATIBLE");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["actual"], "unsupported-version 1");
}

/// A function body is the edit that moves the snapshot and nothing else: the
/// projection is identical, so this isolates staleness from every text rule.
#[test]
fn stale_snapshot_is_a_warning_and_exits_one() {
    let temp = repo();
    write(
        temp.path(),
        "src/auth/token.rs",
        "/// Verifies a bearer token against the signing key.\n\
         pub fn verify_token(token: &str) -> bool {\n    \
         token.len() > 2\n\
         }\n",
    );

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert_eq!(report["status"], "warnings");
    let diagnostic = only(&report, "RR0004_SNAPSHOT_STALE");
    assert_eq!(diagnostic["severity"], "warning");
    assert_eq!(diagnostic["expected"], "fresh");
    assert_eq!(diagnostic["actual"], "stale");
}

/// A repository whose one scope holds a definition too large for a whole page.
fn oversize_repo() -> TempDir {
    let temp = empty_repo();
    let root = temp.path();

    let parameters = (0..400)
        .map(|index| format!("parameter_number_{index:04}: u32"))
        .collect::<Vec<String>>()
        .join(", ");
    write(
        root,
        "src/wide.rs",
        &format!("/// One definition nothing can page.\npub fn enormous_signature({parameters}) -> u32 {{ 0 }}\n"),
    );

    commit_all(root, "seed");
    assert_eq!(code(&run(root, &["map"])), 0, "the fixture must index");
    temp
}

#[test]
fn map_over_budget_is_rr0104() {
    let temp = oversize_repo();
    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    let diagnostic = only(&report, "RR0104_MAP_OVER_BUDGET");
    assert_eq!(diagnostic["severity"], "warning");
    assert_eq!(diagnostic["path"], "src");
}

/// `RR0105` would have said *which* of the two over-budget causes this is. The
/// owning projection collapses both into one crate-private boolean and publishes
/// only the scope list, so both report as `RR0104` and `RR0105` stays reserved —
/// see `RESERVED_RULES`. What is asserted here is the part that matters to a
/// pipeline: an unpageable definition is a warning and not an error, because the
/// map is still written and still answers.
#[test]
fn indivisible_oversize_record_is_rr0104_warning_not_an_error() {
    let temp = oversize_repo();
    let report = json(&run(temp.path(), &["check", "--json"]));

    assert_eq!(report["status"], "warnings");
    assert_eq!(report["counts"]["errors"], 0);
    assert!(
        !rules(&report)
            .iter()
            .any(|rule| rule == "RR0105_MAP_RECORD_INDIVISIBLE"),
        "a reserved rule must emit nothing"
    );
    assert!(RESERVED_RULES.contains(&"RR0105_MAP_RECORD_INDIVISIBLE"));
}

#[test]
fn hand_edited_generated_region_is_rr0102() {
    let temp = repo();
    replace(
        temp.path(),
        "MAP.md",
        "# Repository map: .",
        "# Repository map: edited by hand",
    );

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0102_MAP_INVALID");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["path"], "MAP.md");
}

/// A maintenance aid, not the guard. The guard is `class_of`'s missing `_` arm,
/// which stops the crate compiling when a variant is added; this list is what
/// tells whoever hits that error which rule ids the new variant has to choose
/// between.
#[test]
fn every_conflict_reason_maps_to_a_rule_and_is_an_error() {
    let reasons = [
        ConflictReason::NotOwned,
        ConflictReason::Symlink,
        ConflictReason::MergeConflict,
        ConflictReason::Frontmatter,
        ConflictReason::UnsupportedFormat,
        ConflictReason::Marker,
        ConflictReason::Purpose,
        ConflictReason::GeneratedEdited,
        ConflictReason::Anchor,
        ConflictReason::ManagedIgnore,
        ConflictReason::Unreadable,
        ConflictReason::CaseCollision,
    ];

    for reason in reasons {
        for path in ["src/auth/MAP.md", SYMBOLS_PATH, IGNORE_PATH] {
            let diagnostic = conflict_diagnostic(path, reason);
            assert_eq!(
                diagnostic.severity,
                rr_core::check::Severity::Error,
                "{path} {reason:?}"
            );
            assert!(
                EMITTED_RULES.contains(&diagnostic.rule_id),
                "{path} {reason:?} produced the undeclared rule {}",
                diagnostic.rule_id
            );
            assert_eq!(
                diagnostic.message,
                reason.as_str(),
                "the message must be the owner's own spelling"
            );
            assert_eq!(
                diagnostic.actual.as_deref(),
                Some(reason.as_str()),
                "actual must carry the published serde token"
            );
        }
    }
}

#[test]
fn an_unowned_map_path_is_rr0106_not_rr0102() {
    let temp = repo();
    write(
        temp.path(),
        "MAP.md",
        "my own notes about this repository\n",
    );

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0106_MAP_NOT_WRITABLE");
    assert_eq!(diagnostic["actual"], "not-owned");
    assert!(
        !rules(&report)
            .iter()
            .any(|rule| rule == "RR0102_MAP_INVALID"),
        "an occupied path is not a malformed one: {:?}",
        rules(&report)
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_symbols_path_is_rr0206() {
    let temp = repo();
    let path = temp.path().join(SYMBOLS_PATH);
    std::fs::remove_file(&path).expect("failed to remove SYMBOLS.md");
    std::os::unix::fs::symlink(temp.path().join("src/auth/token.rs"), &path)
        .expect("failed to create the symbolic link");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0206_SYMBOLS_NOT_WRITABLE");
    assert_eq!(diagnostic["actual"], "symlink");
    assert_eq!(diagnostic["path"], SYMBOLS_PATH);
}

#[test]
fn a_managed_ignore_conflict_is_rr0604_before_thirteen_lands() {
    let temp = repo();
    let path = temp.path().join(IGNORE_PATH);
    let mut contents = read(temp.path(), IGNORE_PATH);
    contents.push_str("\n# rr:begin local artifacts\n/extra\n# rr:end local artifacts\n");
    std::fs::write(&path, contents).expect("failed to duplicate the managed block");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0604_LOCAL_IGNORE_INVALID");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(diagnostic["actual"], "managed-ignore");
    for reserved in [
        "RR0601_CONTRACT_MISSING",
        "RR0602_CONTRACT_NOT_OWNED",
        "RR0603_CONTRACT_STALE",
    ] {
        assert!(
            !rules(&report).iter().any(|rule| rule == reserved),
            "{reserved} is reserved until a read-only init planner exists in rr-core"
        );
    }
}

/// A postcard prefix followed by anything is corrupt to the cache's own reader,
/// and this asserts the whole verdict: a warning, exit `1`, and no error — the
/// entry rebuilds itself on the next refresh, so nothing a human must repair.
#[test]
fn valid_prefix_plus_garbage_cache_is_rr0401_warning_only() {
    let temp = repo();
    let facts = temp.path().join(".rr/local/facts");
    let entry = std::fs::read_dir(&facts)
        .expect("the fact cache must exist")
        .filter_map(Result::ok)
        .flat_map(|shard| {
            std::fs::read_dir(shard.path())
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
        })
        .map(|file| file.path())
        .next()
        .expect("the fact cache must hold an entry");
    let mut bytes = std::fs::read(&entry).expect("failed to read the cache entry");
    bytes.extend_from_slice(b"trailing garbage");
    std::fs::write(&entry, &bytes).expect("failed to corrupt the cache entry");

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert_eq!(report["status"], "warnings");
    assert_eq!(report["counts"]["errors"], 0);
    assert_eq!(only(&report, "RR0401_CACHE_CORRUPT")["severity"], "warning");
}

#[test]
fn error_outranks_warning_in_the_exit_code() {
    let temp = repo();
    std::fs::remove_file(temp.path().join("src/store/MAP.md")).expect("failed to remove the map");
    write(
        temp.path(),
        "src/auth/token.rs",
        "/// Verifies a bearer token against the signing key.\n\
         pub fn verify_token(token: &str) -> bool {\n    \
         token.len() > 2\n\
         }\n",
    );

    let output = run(temp.path(), &["check", "--json"]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    assert_eq!(report["status"], "errors");
    assert!(report["counts"]["warnings"].as_u64().unwrap_or(0) > 0);
    assert!(rules(&report)
        .iter()
        .any(|rule| rule == "RR0101_MAP_MISSING"));
    assert!(rules(&report)
        .iter()
        .any(|rule| rule == "RR0004_SNAPSHOT_STALE"));
}

/// The quality refusal is the error, and the absent snapshot is the fatal one.
/// `4` wins, and the report says so even though an error is also present.
#[test]
fn fatal_outranks_error() {
    let temp = repo();
    std::fs::remove_file(temp.path().join(SNAPSHOT)).expect("failed to remove the snapshot");
    let outside = TempDir::new().expect("failed to create a temp dir");
    let absent = outside
        .path()
        .join("never-written.json")
        .display()
        .to_string();

    let output = run(
        temp.path(),
        &["check", "--json", "--quality-report", &absent],
    );

    assert_eq!(code(&output), 4);
    let report = json(&output);
    assert_eq!(report["status"], "snapshot-missing");
    assert_eq!(report["counts"]["fatal"], 1);
    assert_eq!(report["counts"]["errors"], 1);
}

/// D16, and the only rule this command is forbidden to have.
///
/// Tier-2 imports are permanently unresolved by construction, so on a Python
/// repository every import edge is absent. A threshold on that count would fail
/// this repository on a day when nothing is wrong with it.
#[test]
fn no_rule_is_keyed_on_unresolved_count() {
    let temp = empty_repo();
    let root = temp.path();
    for module in 0..6 {
        let mut source = String::new();
        for import in 0..40 {
            let _ = writeln!(source, "import module_{module}_{import}");
        }
        let _ = writeln!(source, "def function_{module}():\n    return 1");
        write(root, &format!("pkg/mod_{module}.py"), &source);
    }
    commit_all(root, "seed");
    assert_eq!(code(&run(root, &["map"])), 0, "the fixture must index");

    let status = json(&run(root, &["status", "--json"]));
    assert!(
        status["unresolved"].as_u64().unwrap_or(0) > 100,
        "the fixture must have a large unresolved population: {status}"
    );

    let output = run(root, &["check", "--json"]);
    assert_eq!(code(&output), 0, "{}", stdout(&output));
    assert_eq!(json(&output)["status"], "ok");
}

/// `2` is clap's, and `rr check` never spends it on a verdict: a script must be
/// able to tell a mistyped flag from a repository with warnings, which is `1`.
#[test]
fn mistyped_flag_still_exits_two_from_clap() {
    let temp = repo();

    assert_eq!(code(&run(temp.path(), &["check", "--quality-repot"])), 2);
    assert_eq!(code(&run(temp.path(), &["check", "--json"])), 0);
}

/// Read-only is the command's contract, not an implementation detail.
///
/// The fingerprint covers every file outside `.git` — the snapshot, the maps,
/// the local caches, the learned routes, the ignore stamp — because the rules
/// that would be tempted to repair something are exactly the ones that read
/// those. A checker that fixed what it found would make its own second run
/// report a clean repository, so a pipeline could never tell "it was fine" from
/// "it was broken and something changed the tree the build is about to ship".
#[test]
fn check_mutates_nothing() {
    let temp = repo();
    let root = temp.path();
    learn(root, "verify token");
    let entry = std::fs::read_dir(root.join(".rr/local/facts"))
        .expect("the fact cache must exist")
        .filter_map(Result::ok)
        .flat_map(|shard| {
            std::fs::read_dir(shard.path())
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
        })
        .map(|file| file.path())
        .next()
        .expect("the fact cache must hold an entry");
    let mut bytes = std::fs::read(&entry).expect("failed to read the cache entry");
    bytes.extend_from_slice(b"trailing garbage");
    std::fs::write(&entry, &bytes).expect("failed to corrupt the cache entry");

    let before = fingerprint(root);
    assert!(before.len() > 6, "the fixture must have files to protect");
    let first = stdout(&run(root, &["check", "--json"]));
    let after = fingerprint(root);
    let second = stdout(&run(root, &["check", "--json"]));

    assert_eq!(before, after, "rr check wrote to the repository");
    assert_eq!(first, second, "two runs of one repository must agree");
}

/// Every file outside `.git`, as `(repo-relative path, contents)`.
///
/// `.git` is excluded because reading a repository's status is Git's own
/// business and may touch its index; nothing under `.git` is an rr artifact.
fn fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .expect("failed to read a directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            let relative = path
                .strip_prefix(root)
                .expect("every path is under the root")
                .to_string_lossy()
                .into_owned();
            if relative == ".git" {
                continue;
            }
            if path.is_dir() && !path.is_symlink() {
                walk(root, &path, out);
            } else {
                out.push((relative, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }

    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Total, so the printed order cannot depend on the order the rules ran in.
#[test]
fn diagnostics_sort_is_total_and_stable() {
    let temp = repo();
    let root = temp.path();
    std::fs::remove_file(root.join("src/store/MAP.md")).expect("failed to remove the map");
    write(root, "src/auth/MAP.md", "somebody else's notes\n");
    write(
        root,
        "src/auth/token.rs",
        "/// Verifies a bearer token against the signing key.\n\
         pub fn verify_token(token: &str) -> bool {\n    \
         token.len() > 2\n\
         }\n",
    );

    let first = stdout(&run(root, &["check", "--json"]));
    let second = stdout(&run(root, &["check", "--json"]));
    assert_eq!(first, second);

    let report: Value = serde_json::from_str(&first).expect("stdout was not one JSON object");
    let weights: Vec<u8> = report["diagnostics"]
        .as_array()
        .expect("diagnostics must be an array")
        .iter()
        .map(|diagnostic| match diagnostic["severity"].as_str() {
            Some("fatal") => 0,
            Some("error") => 1,
            Some("warning") => 2,
            other => panic!("unknown severity {other:?}"),
        })
        .collect();
    assert!(
        weights.len() >= 3,
        "the fixture must raise several findings"
    );
    assert!(
        weights.windows(2).all(|pair| pair[0] <= pair[1]),
        "the heaviest finding must print first: {weights:?}"
    );
}

#[test]
fn text_and_json_agree_on_status_and_counts() {
    let temp = repo();
    let root = temp.path();
    std::fs::remove_file(root.join("src/store/MAP.md")).expect("failed to remove the map");

    let report = json(&run(root, &["check", "--json"]));
    let text = stdout(&run(root, &["check"]));

    let summary = format!(
        "check: {} · fatal: {} · errors: {} · warnings: {}",
        report["status"].as_str().unwrap_or_default(),
        report["counts"]["fatal"],
        report["counts"]["errors"],
        report["counts"]["warnings"]
    );
    assert_eq!(text.lines().next(), Some(summary.as_str()));
    for rule in rules(&report) {
        assert!(text.contains(&rule), "the human report omits {rule}");
    }
}

#[test]
fn an_unreadable_route_cache_is_rr0301_warning() {
    let temp = repo();
    let root = temp.path();
    learn(root, "verify token");
    std::fs::write(root.join(ROUTES_PATH), "not a routes file\n")
        .expect("failed to overwrite the route cache");

    let output = run(root, &["check", "--json"]);

    assert_eq!(
        code(&output),
        1,
        "a self-healing local cache is a warning, not an error"
    );
    let report = json(&output);
    let diagnostic = only(&report, "RR0301_ROUTES_INVALID");
    assert_eq!(diagnostic["severity"], "warning");
    assert_eq!(diagnostic["path"], ROUTES_PATH);
    assert_eq!(
        diagnostic["actual"], "the header is not the one rr writes",
        "the fault's own spelling is what the closed enum buys"
    );
}

#[test]
fn a_route_anchor_the_snapshot_lost_is_rr0302() {
    let temp = repo();
    let root = temp.path();
    learn(root, "verify token");
    replace(root, ROUTES_PATH, "#verify_token", "#vanished_symbol");

    let output = run(root, &["check", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    let diagnostic = only(&report, "RR0302_ROUTE_ANCHOR_MISSING");
    assert_eq!(diagnostic["severity"], "warning");
    assert!(
        diagnostic["anchor"]
            .as_str()
            .unwrap_or_default()
            .ends_with("#vanished_symbol"),
        "the anchor travels verbatim: {diagnostic}"
    );
}

/// One diagnostic for the whole table, because `api_identity` is the identity of
/// the corpus and not of a scope: every record carries the same value, so a
/// thousand routes go stale as one event rather than as a thousand defects.
#[test]
fn a_route_cache_from_another_corpus_is_one_rr0303() {
    let temp = repo();
    let root = temp.path();
    learn(root, "verify token");
    learn(root, "serialize entry");

    let learned = read(root, ROUTES_PATH);
    let records = learned
        .lines()
        .filter(|line| line.contains('\t'))
        .filter(|line| !line.starts_with("<!--"))
        .count();
    assert_eq!(records, 2, "the fixture must learn two routes: {learned}");
    let identity = learned
        .lines()
        .find_map(|line| {
            line.split('\t')
                .nth(3)
                .filter(|field| field.starts_with("blake3:"))
        })
        .expect("a record must carry an api_identity")
        .to_owned();
    replace(root, ROUTES_PATH, &identity, CORPUS);

    let output = run(root, &["check", "--json"]);

    assert_eq!(code(&output), 1);
    let report = json(&output);
    let diagnostic = only(&report, "RR0303_ROUTE_API_STALE");
    assert_eq!(diagnostic["severity"], "warning");
    assert_eq!(diagnostic["expected"], identity);
    assert_eq!(diagnostic["actual"], CORPUS);
}

/// Refused on the file's declared size, before a byte of it is read.
///
/// The point is not that a large report is rejected but that nothing is
/// allocated to find out: an operator who typed the path of a database dump
/// should get a diagnostic, not a process that reserves the file's length first.
#[test]
fn quality_report_over_sixteen_mib_is_refused_before_allocation() {
    let temp = repo();
    let outside = TempDir::new().expect("failed to create a temp dir");
    let path = outside.path().join("huge.json");
    let size = usize::try_from(MAX_QUALITY_REPORT_BYTES).expect("the cap fits in a usize") + 1;
    std::fs::write(&path, vec![b'{'; size]).expect("failed to write the oversize report");

    let output = run(
        temp.path(),
        &[
            "check",
            "--json",
            "--quality-report",
            &path.display().to_string(),
        ],
    );

    assert_eq!(code(&output), 3);
    let report = json(&output);
    let diagnostic = only(&report, "RR0501_QUALITY_REPORT_INVALID");
    assert_eq!(diagnostic["actual"], "too-large");
    assert!(
        report.get("quality").is_none(),
        "a refused report must contribute no summary"
    );
}

#[test]
fn quality_report_with_trailing_bytes_is_rr0501() {
    let temp = repo();
    let outside = TempDir::new().expect("failed to create a temp dir");
    let path = outside.path().join("trailing.json");
    std::fs::write(
        &path,
        format!("{{\"schema_version\":1,\"manifest_digest\":\"{CORPUS}\",\"findings\":[]}} extra"),
    )
    .expect("failed to write the report");

    let output = run(
        temp.path(),
        &[
            "check",
            "--json",
            "--quality-report",
            &path.display().to_string(),
        ],
    );

    assert_eq!(code(&output), 3);
    assert_eq!(
        only(&json(&output), "RR0501_QUALITY_REPORT_INVALID")["actual"],
        "trailing-bytes"
    );
}

#[test]
fn unknown_quality_schema_version_fails_closed() {
    let temp = repo();
    let outside = TempDir::new().expect("failed to create a temp dir");
    let path = outside.path().join("future.json");
    std::fs::write(
        &path,
        format!("{{\"schema_version\":99,\"manifest_digest\":\"{CORPUS}\",\"findings\":[]}}"),
    )
    .expect("failed to write the report");

    let output = run(
        temp.path(),
        &[
            "check",
            "--json",
            "--quality-report",
            &path.display().to_string(),
        ],
    );

    assert_eq!(code(&output), 3);
    assert_eq!(
        only(&json(&output), "RR0501_QUALITY_REPORT_INVALID")["actual"],
        "unsupported-schema-version"
    );
}

/// `--quality-report` adds evidence; it never answers for the repository.
#[test]
fn quality_report_does_not_suppress_repository_rules() {
    let temp = repo();
    let root = temp.path();
    std::fs::remove_file(root.join("src/store/MAP.md")).expect("failed to remove the map");
    let outside = TempDir::new().expect("failed to create a temp dir");
    let path = quality_report(
        outside.path(),
        "quality.json",
        "{\"rule_id\":\"RR0504_PERFORMANCE_REGRESSION\",\"blocked\":true,\
         \"message\":\"cold map regressed\",\"expected\":\"1.05\",\"actual\":\"1.31\"},\
         {\"rule_id\":\"RR0503_ROUTING_QUALITY_REGRESSION\",\"blocked\":false,\
         \"message\":\"routing held\"}",
    );

    let output = run(root, &["check", "--json", "--quality-report", &path]);

    assert_eq!(code(&output), 3);
    let report = json(&output);
    assert!(rules(&report)
        .iter()
        .any(|rule| rule == "RR0101_MAP_MISSING"));
    assert!(rules(&report)
        .iter()
        .any(|rule| rule == "RR0504_PERFORMANCE_REGRESSION"));
    assert_eq!(report["quality"]["findings"], 2);
    assert_eq!(report["quality"]["blocked"], 1);
    assert_eq!(report["quality"]["manifest_digest"], CORPUS);
}

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use rr_core::render::decode_anchor;
use tempfile::TempDir;

fn setup_test_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    run_cmd(root, "git", &["init"]);
    run_cmd(root, "git", &["config", "user.email", "test@example.com"]);
    run_cmd(root, "git", &["config", "user.name", "Tester"]);

    let auth_dir = root.join("src").join("auth");
    fs::create_dir_all(&auth_dir).unwrap();

    fs::write(
        auth_dir.join("token.rs"),
        b"pub fn verify_token() -> bool { true }\n",
    )
    .unwrap();

    fs::write(auth_dir.join("session.rs"), b"pub fn session() {}\n").unwrap();

    let src_dir = root.join("src");
    fs::write(src_dir.join("session.rs"), b"pub fn session() {}\n").unwrap();

    run_cmd(root, "git", &["add", "."]);
    run_cmd(root, "git", &["commit", "-m", "init"]);

    let map_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(root)
        .arg("map")
        .output()
        .unwrap();
    assert!(map_output.status.success());

    temp
}

/// A repository whose definitions carry enough prose for the lexical fallback
/// to have something to rank.
fn setup_lexical_repo() -> TempDir {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    run_cmd(root, "git", &["init"]);
    run_cmd(root, "git", &["config", "user.email", "test@example.com"]);
    run_cmd(root, "git", &["config", "user.name", "Tester"]);

    let auth = root.join("src").join("auth");
    let store = root.join("src").join("store");
    fs::create_dir_all(&auth).unwrap();
    fs::create_dir_all(&store).unwrap();

    fs::write(
        auth.join("session.rs"),
        b"/// Creates a session for a caller that has already been authenticated.\n\
          pub fn create_session(user: UserId) -> Session {\n    \
          Session::new(user)\n\
          }\n\
          \n\
          /// Ends a session and forgets everything it held.\n\
          pub fn end_session(session: Session) {}\n",
    )
    .unwrap();

    fs::write(
        store.join("serialize.rs"),
        b"/// Encodes an entry into its wire representation.\n\
          pub fn serialize_entry(entry: Entry) -> Vec<u8> {\n    \
          Vec::new()\n\
          }\n\
          \n\
          /// Decodes an entry from its wire representation.\n\
          pub fn deserialize_entry(bytes: &[u8]) -> Entry {\n    \
          Entry::default()\n\
          }\n",
    )
    .unwrap();

    run_cmd(root, "git", &["add", "."]);
    run_cmd(root, "git", &["commit", "-m", "init"]);
    let map = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(root)
        .arg("map")
        .output()
        .unwrap();
    assert!(map.status.success());
    temp
}

fn query(repo: &TempDir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .arg("query")
        .args(args)
        .output()
        .unwrap()
}

fn run_cmd(dir: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {program} {args:?}"
    );
}

#[test]
fn test_query_symbol_direct_text() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_file_direct_text() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "src/auth/token.rs"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_candidates_text() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "session"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "source candidates:\n1. src/auth/session.rs#session\n2. src/session.rs#session\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_path_filter_text() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "--path", "src/auth/session.rs", "session"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/session.rs#session\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_none_not_found_text() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "nonexistent_function"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "NO ANCHOR (index has no match); try: rr map\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_json_direct() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "--json", "verify_token"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout_str = String::from_utf8(output.stdout).unwrap();
    assert!(stdout_str.ends_with('\n'));
    assert_eq!(stdout_str.lines().count(), 1);

    let val: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();
    assert_eq!(val["v"], 1);
    assert_eq!(val["result"], "direct");
    assert_eq!(val["pipeline"], "exact");
    assert_eq!(val["confidence"], 1.0);
    assert_eq!(val["anchor"]["path"], "src/auth/token.rs");
    assert_eq!(val["anchor"]["symbol"], "verify_token");
    assert_eq!(val["anchor"]["lines"], serde_json::json!([1, 1]));
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_json_candidates() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "--json", "session"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout_str = String::from_utf8(output.stdout).unwrap();
    assert!(stdout_str.ends_with('\n'));
    assert_eq!(stdout_str.lines().count(), 1);

    let val: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();
    assert_eq!(val["v"], 1);
    assert_eq!(val["result"], "candidates");
    assert_eq!(val["pipeline"], "exact");
    let candidates = val["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["anchor"]["path"], "src/auth/session.rs");
    assert_eq!(candidates[0]["anchor"]["symbol"], "session");
    assert!(candidates[0]["confidence"].is_null());
    assert_eq!(candidates[1]["anchor"]["path"], "src/session.rs");
    assert_eq!(candidates[1]["anchor"]["symbol"], "session");
    assert!(candidates[1]["confidence"].is_null());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_query_json_none() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "--json", "nonexistent_symbol"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stdout_str = String::from_utf8(output.stdout).unwrap();
    assert!(stdout_str.ends_with('\n'));
    assert_eq!(stdout_str.lines().count(), 1);

    let val: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();
    assert_eq!(val["v"], 1);
    assert_eq!(val["result"], "none");
    assert_eq!(
        val["pipeline"], "lexical",
        "an exact miss falls through to the lexical pipeline, which owns the abstention"
    );
    assert_eq!(val["reason"], "not_found");
    assert!(val.get("anchor").is_none());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_decode_text_anchor_matches_json_anchor() {
    let repo = setup_test_repo();

    let text_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "verify_token"])
        .output()
        .unwrap();
    let text_line = String::from_utf8(text_output.stdout).unwrap();
    let raw_anchor = text_line
        .strip_prefix("FINAL SOURCE ANCHOR (copy exactly): ")
        .unwrap()
        .trim_end_matches('\n');
    let (decoded_path, decoded_sym) = decode_anchor(raw_anchor).unwrap();

    let json_output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "--json", "verify_token"])
        .output()
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();

    assert_eq!(
        decoded_path.as_str(),
        val["anchor"]["path"].as_str().unwrap()
    );
    assert_eq!(decoded_sym.as_deref(), val["anchor"]["symbol"].as_str());
}

#[test]
fn test_query_empty_error() {
    let repo = setup_test_repo();
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .current_dir(repo.path())
        .args(["query", "   "])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr_str = String::from_utf8(output.stderr).unwrap();
    assert!(stderr_str.starts_with("rr: query:"));
}

#[test]
fn query_contract_lexical_direct_answers_a_natural_language_question() {
    let repo = setup_lexical_repo();
    let output = query(&repo, &["--json", "how do we create a session?"]);

    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["v"], 1);
    assert_eq!(value["result"], "direct");
    assert_eq!(value["pipeline"], "lexical");
    assert_eq!(value["anchor"]["path"], "src/auth/session.rs");
    assert_eq!(value["anchor"]["symbol"], "create_session");
    let confidence = value["confidence"].as_f64().unwrap();
    assert!(
        (0.0..1.0).contains(&confidence),
        "a lexical answer reports its normalized margin, never the certainty of an exact \
         match: {confidence}"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn query_contract_lexical_direct_prints_one_anchor_line() {
    let repo = setup_lexical_repo();
    let output = query(&repo, &["how do we create a session?"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/session.rs#create_session\n",
        "the text contract is identical whichever pipeline answered"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn query_contract_lexical_candidates_carry_no_confidence() {
    let repo = setup_lexical_repo();
    let output = query(&repo, &["--json", "encode an entry to bytes"]);

    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "candidates");
    assert_eq!(value["pipeline"], "lexical");
    let candidates = value["candidates"].as_array().unwrap();
    assert!(
        (1..=3).contains(&candidates.len()),
        "the public contract carries at most three candidates, got {}",
        candidates.len()
    );
    assert!(candidates
        .iter()
        .all(|candidate| candidate["confidence"].is_null()));
    assert!(output.stderr.is_empty());
}

#[test]
fn query_contract_lexical_abstains_on_incidental_evidence() {
    let repo = setup_lexical_repo();
    let output = query(&repo, &["quantum flux capacitor"]);

    assert_eq!(output.status.code(), Some(3));
    let output = query(&repo, &["--json", "quantum flux capacitor"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result"], "none");
    assert_eq!(value["pipeline"], "lexical");
    assert_eq!(value["reason"], "not_found");
    assert!(value.get("anchor").is_none());
    assert!(value.get("confidence").is_none());

    let output = query(&repo, &["--json", "the wire representation"]);
    assert_eq!(output.status.code(), Some(3));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["reason"], "low_confidence",
        "a query whose words are indexed but whose best match is too weak abstains for a \
         different reason than one whose words are unknown"
    );
}

#[test]
fn query_contract_lexical_repeats_byte_for_byte_across_processes() {
    let repo = setup_lexical_repo();
    let first = query(&repo, &["--json", "how do we create a session?"]).stdout;
    for run in 0..4 {
        let again = query(&repo, &["--json", "how do we create a session?"]).stdout;
        assert_eq!(first, again, "run {run} printed a different answer");
    }
}

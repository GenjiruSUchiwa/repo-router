//! The learned route cache, end to end: `.rr/ROUTES.md` as a user meets it.
//!
//! The unit tests in `rr-core` pin what the file *is*. These pin what it is
//! *for*: a second identical question answered without ranking, an answer that
//! survives the edits which change nothing about a symbol's public shape, and
//! an answer that is gone the moment that shape moves. The staleness rule is
//! the interesting half — a route dies for a rename anywhere in its directory,
//! which is coarse on purpose, so the coarseness is asserted rather than
//! discovered.

mod common;

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

use common::{code, commit_all, empty_repo, git, json, read, run, stdout, write};

const ROUTES: &str = ".rr/ROUTES.md";

/// Two public symbols in one directory, a third somewhere else, each carrying
/// enough prose for the lexical pipeline to answer a question about it.
///
/// Two in `src/auth/` is not incidental: it is what makes
/// `a_neighbour_rename_retires_the_route_too` a test of scope granularity
/// rather than of renaming the target.
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

/// One question, asked as a program would ask it.
fn ask(root: &Path, question: &str) -> Value {
    json(&run(root, &["query", "--json", question]))
}

/// Commits the tree and brings the index back in step with it.
///
/// Both halves matter: `rr query` refuses a snapshot whose HEAD moved, so a
/// test that edits a file and forgets to refresh proves nothing about routes.
fn commit_and_refresh(root: &Path) -> String {
    commit_all(root, "edit");
    let output = run(root, &["refresh", "--verbose"]);
    assert_eq!(code(&output), 0, "refresh failed: {}", stdout(&output));
    stdout(&output)
}

/// The whole point of the feature, in the smallest form it has.
#[test]
fn a_second_identical_query_answers_from_the_route_cache() {
    let temp = repo();
    let root = temp.path();

    let first = ask(root, "verify token");
    assert_eq!(first["result"], "direct");
    assert_eq!(first["pipeline"], "lexical");

    let second = ask(root, "verify token");
    assert_eq!(second["pipeline"], "route", "the second ask was not cached");
    assert_eq!(
        second["anchor"], first["anchor"],
        "a cached answer that differs from the answer it cached is worse than no cache"
    );
    assert_eq!(second["confidence"], first["confidence"]);
}

/// The key is the words, lowercased and sorted — so the same question typed in
/// another order, in another case, is the same question.
#[test]
fn a_reordered_question_finds_the_same_route() {
    let temp = repo();
    let root = temp.path();

    let first = ask(root, "verify token");
    let second = ask(root, "TOKEN Verify");

    assert_eq!(second["pipeline"], "route");
    assert_eq!(second["anchor"], first["anchor"]);
}

/// D8's first consequence: a purpose is prose a human wrote, and prose is not
/// part of a symbol's public shape. This is why a record carries no
/// `generated_hash`.
#[test]
fn a_route_survives_editing_a_purpose_slot() {
    let temp = repo();
    let root = temp.path();
    ask(root, "verify token");

    let map = read(root, "src/auth/MAP.md");
    let edited = map.replace(
        "TODO: describe this scope.",
        "Everything about bearer tokens lives here.",
    );
    assert_ne!(edited, map, "the fixture map must carry a purpose slot");
    write(root, "src/auth/MAP.md", &edited);
    commit_and_refresh(root);

    let after = ask(root, "verify token");
    assert_eq!(
        after["pipeline"], "route",
        "editing prose retired a route: {after}"
    );
}

/// D8's second consequence, and the test that fails the moment anyone adds a
/// line column to a record: the answer's lines are the *new* ones, read live
/// from the snapshot, while the route itself never noticed.
#[test]
fn a_route_survives_moving_a_function_down_its_file() {
    let temp = repo();
    let root = temp.path();

    let before = ask(root, "verify token");
    let lines_before = before["anchor"]["lines"].clone();

    let source = read(root, "src/auth/token.rs");
    write(root, "src/auth/token.rs", &format!("\n\n\n{source}"));
    commit_and_refresh(root);

    let after = ask(root, "verify token");
    assert_eq!(
        after["pipeline"], "route",
        "moving a definition retired a route: {after}"
    );
    assert_eq!(after["anchor"]["path"], before["anchor"]["path"]);
    assert_ne!(
        after["anchor"]["lines"], lines_before,
        "a cached route must report where the symbol is now, not where it was"
    );
}

/// The failure the cache exists to avoid: an answer that points at a name the
/// repository no longer has.
#[test]
fn a_renamed_symbol_retires_its_route() {
    let temp = repo();
    let root = temp.path();
    ask(root, "verify token");
    assert!(read(root, ROUTES).contains("verify_token"));

    let source = read(root, "src/auth/token.rs");
    write(
        root,
        "src/auth/token.rs",
        &source.replace("verify_token", "check_token"),
    );
    let verbose = commit_and_refresh(root);
    assert!(
        verbose.contains("1 routes retired"),
        "the refresh report never mentioned the retirement: {verbose}"
    );

    let after = ask(root, "verify token");
    assert_ne!(
        after["pipeline"], "route",
        "a route to a symbol that no longer exists was served: {after}"
    );
}

/// D8's third consequence, asserted so that the coarseness is a documented
/// promise rather than a surprise: an `api_hash` covers a whole directory, so a
/// rename next door retires this route too. Over-invalidating costs one ranked
/// query; under-invalidating costs a wrong answer.
#[test]
fn a_neighbour_rename_retires_the_route_too() {
    let temp = repo();
    let root = temp.path();
    ask(root, "verify token");

    let neighbour = read(root, "src/auth/keys.rs");
    write(
        root,
        "src/auth/keys.rs",
        &neighbour.replace("rotate_signing_key", "roll_signing_key"),
    );
    commit_and_refresh(root);

    let after = ask(root, "verify token");
    assert_ne!(
        after["pipeline"], "route",
        "a rename in the same scope left the route live: {after}"
    );
}

/// The qualifier is part of the question and is not part of the key, so caching
/// one would let a later unqualified question receive a qualified answer.
#[test]
fn a_path_qualified_query_is_neither_cached_nor_answered_from_cache() {
    let temp = repo();
    let root = temp.path();

    ask(root, "verify token");
    let learned = read(root, ROUTES);

    let qualified = json(&run(
        root,
        &[
            "query",
            "--json",
            "--path",
            "src/auth/token.rs",
            "verify token",
        ],
    ));
    assert_ne!(
        qualified["pipeline"], "route",
        "a qualified question was answered from an unqualified route: {qualified}"
    );
    assert_eq!(
        read(root, ROUTES),
        learned,
        "a qualified question wrote itself into the cache"
    );
}

/// Only a direct answer to a symbol is a route: candidates name no one symbol,
/// so there is no `api_hash` that could ever mark them stale.
#[test]
fn a_candidates_result_writes_no_route() {
    let temp = repo();
    let root = temp.path();

    ask(root, "verify token");
    let learned = read(root, ROUTES);

    let ambiguous = run(root, &["query", "--json", "quantum flux capacitor"]);
    assert_ne!(code(&ambiguous), 0, "the fixture must not answer this");
    assert_eq!(
        read(root, ROUTES),
        learned,
        "an answer that named no symbol was filed anyway"
    );
}

/// Amendment A, made checkable: the managed pattern covers the file on its own,
/// independently of the `*` stamp that covers it today. Replacing
/// `.rr/.gitignore` with the block alone is what tells the two apart.
#[test]
fn the_routes_file_is_ignored_by_git() {
    let temp = repo();
    let root = temp.path();
    ask(root, "verify token");

    write(
        root,
        ".rr/.gitignore",
        "# rr:begin local artifacts\n/ROUTES.md\n/SYMBOLS.md\n/local/\n\
         # rr:end local artifacts\n",
    );

    let output = Command::new("git")
        .args(["check-ignore", "-v", ROUTES])
        .current_dir(root)
        .output()
        .expect("git check-ignore");
    assert_eq!(
        output.status.code(),
        Some(0),
        "the managed block alone does not hide the cache"
    );
    let matched = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        matched.contains("/ROUTES.md"),
        "some other rule hid the file, so this proves nothing: {matched}"
    );

    // And it stays out of a commit: an artifact that is machine-local is only
    // machine-local if `git add -A` cannot pick it up.
    git(root, &["add", "-A"]);
    let staged = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(root)
        .output()
        .expect("git diff --cached");
    let staged = String::from_utf8_lossy(&staged.stdout).into_owned();
    assert!(
        !staged.contains("ROUTES.md"),
        "the cache was staged for commit: {staged}"
    );
}

/// The test that fails without the lock: two processes learning at once must
/// not lose one another's record to a read-modify-write race.
#[test]
fn two_concurrent_queries_both_record_their_route() {
    let temp = repo();
    let root = temp.path();

    let children: Vec<_> = ["verify token", "rotate signing key", "serialize entry"]
        .iter()
        .map(|question| {
            Command::new(env!("CARGO_BIN_EXE_rr"))
                .args(["query", "--json", question])
                .current_dir(root)
                .spawn()
                .expect("spawn rr query")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("join rr query").success());
    }

    let file = read(root, ROUTES);
    for key in ["token verify", "key rotate signing", "entry serialize"] {
        assert!(
            file.lines()
                .any(|line| line.starts_with(&format!("{key}\t"))),
            "the route for {key:?} was lost to a concurrent writer:\n{file}"
        );
    }
}

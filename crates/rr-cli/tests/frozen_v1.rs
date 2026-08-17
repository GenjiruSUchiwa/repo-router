//! What v1 of the `rr query` surface promises, and the tests that hold it.
//!
//! **v1 is closed.** No member is added, removed, renamed or reordered; no
//! variant is added to a serialized enum; no exit code is re-meant; no byte of a
//! text marker changes. Anything else ships as `v: 2` beside v1 for one minor
//! release, so a consumer has a release in which both answers exist and can be
//! moved without being broken first.
//!
//! "Additive is safe" is **false** here. `tests/query.schema.json` says
//! `additionalProperties: false` at every object, so one added member does not
//! extend the answer — it makes every conforming consumer reject the answer,
//! silently, inside an agent's parser, on a response whose other members are all
//! correct. That failure is worse than a version bump and invisible in the
//! output, which is why v1 grows a sibling rather than a member.
//!
//! `rr_core::json_contract` is where the promise is stated normatively; this
//! file is where it is checked. Five things are frozen and each is checked here
//! or, where another suite already owns the comparison, named below:
//!
//! 1. the schema file, byte for byte ([`V1_SCHEMA_SHA256`]);
//! 2. the member set of every serialized shape — the three response shapes in
//!    `query_contract.rs`, the two nested ones here;
//! 3. the `v: 1` literal, at all three of its sites;
//! 4. the closed marker inventory, `rr_core::render::marker::ALL`;
//! 5. the exit codes: `0` direct, `2` candidates, `3` none, `4` refused, and `1`
//!    for an error, which must never collide with the `2` that means the answer
//!    arrived and is not one to act on.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rr_core::oid::Oid;
use rr_core::render::marker;
use rr_core::verify::MAX_SOURCE_LINES;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use common::{code, commit_all, empty_repo, json, run, stderr, stdout, write};

/// The SHA-256 of `tests/query.schema.json`, as `shasum -a 256` prints it.
///
/// Recompute with `shasum -a 256 crates/rr-cli/tests/query.schema.json`. It
/// covers the file's own bytes, so a reader can check the published schema
/// against this line without building anything.
///
/// This is not a checksum of whatever the file currently holds — it is the
/// statement that the file currently held is the published one. A v2 therefore
/// adds a second constant beside this one; editing this value in place is how v1
/// silently stops being v1.
const V1_SCHEMA_SHA256: &str = "2dc6a9daa904176c4e443fba167af509c3ece7d62992f095915f1ff7b912fcf8";

/// A query no corpus in this file indexes a word of, so the answer is a miss
/// rather than a weak match.
const UNKNOWN_QUERY: &str = "zzqqxx_nothing";

/// A query whose every word is indexed and whose best match still does not clear
/// the abstention margin, on [`low_confidence_repo`].
const WEAK_QUERY: &str = "the wire representation";

fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/query.schema.json")
}

/// The published schema, read from the repository rather than restated here.
///
/// This pair of readers is spelled again in `query_contract.rs`, which owns the
/// comparison for the three response shapes. Each integration test file is its
/// own binary, so the only way to share them would be `tests/common/mod.rs`,
/// which deliberately carries the mechanics of running `rr` and no knowledge of
/// any contract.
fn published_schema() -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(schema_path()).unwrap()).unwrap()
}

/// The member names one closed shape declares, sorted.
///
/// Asserts on the way past that the shape is closed and that every member it
/// declares is required: set equality between "declared" and "emitted" only
/// means something while neither side may omit a member.
fn declared_members(schema: &serde_json::Value, shape: &str) -> Vec<String> {
    let object = &schema["$defs"][shape];
    assert_eq!(
        object["additionalProperties"],
        serde_json::json!(false),
        "{shape} must stay closed; an open object would let a member be added \
         without this file noticing"
    );
    let mut properties: Vec<String> = object["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    properties.sort();
    let mut required: Vec<String> = object["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect();
    required.sort();
    assert_eq!(
        properties, required,
        "{shape} declares an optional member, so comparing member sets is no \
         longer the whole contract for it"
    );
    properties
}

/// The member names one rendered value actually carries, sorted.
fn rendered_members(value: &serde_json::Value) -> Vec<String> {
    let mut members: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    members.sort();
    members
}

/// `count` distinct statement lines, so a definition can be given an exact
/// length without repeating one line the extractor might fold together.
fn numbered_body(count: u32) -> String {
    (0..count).fold(String::new(), |mut body, n| {
        use std::fmt::Write as _;
        let _ = writeln!(body, "    let v{n} = {n};");
        body
    })
}

/// A definition of exactly [`MAX_SOURCE_LINES`] lines, under one context line.
///
/// The anchor fits the line budget whole, which leaves no room for the context
/// line above it: the one shape that reports `SOURCE CONTEXT CLIPPED` without
/// also reporting truncation.
fn clipped_source() -> String {
    let body = numbered_body(MAX_SOURCE_LINES - 3);
    format!(
        "// one context line, which the line budget will have no room for\n\
         pub fn clipped_anchor() -> bool {{\n{body}    true\n}}\n"
    )
}

/// A definition longer than the line budget, so its packet is truncated.
fn long_source() -> String {
    let body = numbered_body(MAX_SOURCE_LINES + 80);
    format!("pub fn long_anchor() -> bool {{\n{body}    true\n}}\n")
}

/// One repository every marker of the closed inventory can be provoked from.
///
/// One fixture rather than seven, because the markers are a set and the test
/// over them asserts that the set was covered exactly. Nothing here asserts a
/// score or a candidate order, so a file added for one marker cannot break the
/// assertion made for another.
fn frozen_repo() -> TempDir {
    let repo = empty_repo();
    let root = repo.path();
    write(
        root,
        "src/auth/token.rs",
        "pub fn verify_token() -> bool { true }\n",
    );
    write(root, "src/auth/session.rs", "pub fn session() {}\n");
    write(root, "src/session.rs", "pub fn session() {}\n");
    write(root, "src/tail.rs", "pub fn no_newline() -> u8 { 7 }");
    write(root, "src/clipped.rs", &clipped_source());
    write(root, "src/long.rs", &long_source());
    commit_all(root, "init");
    let mapped = run(root, &["map"]);
    assert_eq!(code(&mapped), 0, "map failed: {}", stderr(&mapped));
    repo
}

/// A repository whose prose gives the lexical pipeline something to be unsure
/// about, so that the low-confidence abstention has a way to be reached.
fn low_confidence_repo() -> TempDir {
    let repo = empty_repo();
    let root = repo.path();
    write(
        root,
        "src/store/serialize.rs",
        "/// Encodes an entry into its wire representation.\n\
         pub fn serialize_entry(entry: Entry) -> Vec<u8> {\n    \
         Vec::new()\n\
         }\n\
         \n\
         /// Decodes an entry from its wire representation.\n\
         pub fn deserialize_entry(bytes: &[u8]) -> Entry {\n    \
         Entry::default()\n\
         }\n",
    );
    commit_all(root, "init");
    let mapped = run(root, &["map"]);
    assert_eq!(code(&mapped), 0, "map failed: {}", stderr(&mapped));
    repo
}

/// Whether one output prints a marker exactly, at the start of a line.
///
/// Both halves matter: the bytes have to be present, and they have to begin a
/// line, because every marker is a line prefix and a consumer finds them by
/// reading lines.
fn emits(text: &str, marker: &str) -> bool {
    let line = marker.strip_suffix('\n').unwrap_or(marker);
    text.contains(marker) && text.lines().any(|candidate| candidate.starts_with(line))
}

/// Records that one output carried the markers it was expected to carry.
fn observe(seen: &mut BTreeSet<&'static str>, text: &str, markers: &[&'static str]) {
    for marker in markers {
        assert!(
            emits(text, marker),
            "{marker:?} is not printed verbatim at the start of a line:\n{text}"
        );
        seen.insert(marker);
    }
}

#[test]
fn query_schema_file_is_unchanged() {
    let bytes = fs::read(schema_path()).unwrap();
    let digest = Oid::from_raw(&Sha256::digest(&bytes)).unwrap().to_hex();

    assert_eq!(
        digest, V1_SCHEMA_SHA256,
        "v1 is frozen; ship v2 alongside it for one minor release"
    );
}

/// The two nested shapes no other suite compares against the schema.
///
/// `DirectResult`, `CandidatesResult` and `NoneResult` are covered by
/// `query_contract.rs::query_contract_json_carries_exactly_the_members_the_schema_declares`,
/// which already asserts set equality — not containment — between each rendered
/// response and the schema. Repeating it here would give one question two
/// answers that could disagree. What that test does not reach is the two shapes
/// nested inside those responses: an `Anchor`, in both spellings the schema
/// gives it, and a `CandidateItem`.
#[test]
fn direct_json_keys_match_the_schema_exactly() {
    let repo = frozen_repo();
    let root = repo.path();
    let schema = published_schema();

    let branches: Vec<String> = schema["$defs"]["Anchor"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|branch| branch["$ref"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        branches,
        vec!["#/$defs/FileAnchor", "#/$defs/SymbolAnchor"],
        "an anchor has exactly two spellings; a third would be a new variant of \
         a serialized shape"
    );

    let symbol = json(&run(root, &["query", "--json", "verify_token"]));
    assert_eq!(
        rendered_members(&symbol["anchor"]),
        declared_members(&schema, "SymbolAnchor"),
        "the symbol anchor the renderer produces must carry exactly the members \
         tests/query.schema.json declares"
    );

    let file = json(&run(root, &["query", "--json", "src/tail.rs"]));
    assert!(
        file["anchor"]["symbol"].is_null(),
        "this case only covers the file spelling while the anchor names no symbol"
    );
    assert_eq!(
        rendered_members(&file["anchor"]),
        declared_members(&schema, "FileAnchor"),
        "a file anchor carries the same members as a symbol anchor, with two of \
         them null; dropping them instead would be a second anchor shape"
    );

    let candidates = json(&run(root, &["query", "--json", "session"]));
    let items = candidates["candidates"].as_array().unwrap().clone();
    assert!(
        !items.is_empty(),
        "this case needs a candidate list to have members to compare"
    );
    for item in &items {
        assert_eq!(
            rendered_members(item),
            declared_members(&schema, "CandidateItem"),
            "every candidate carries exactly the members the schema declares"
        );
    }
}

#[test]
fn version_field_is_the_literal_one() {
    let repo = frozen_repo();
    let root = repo.path();

    for (shape, query) in [
        ("direct", "verify_token"),
        ("candidates", "session"),
        ("none", UNKNOWN_QUERY),
    ] {
        let response = json(&run(root, &["query", "--json", query]));
        assert_eq!(response["result"], shape, "wrong shape for {query:?}");
        assert_eq!(
            response["v"],
            serde_json::json!(1),
            "the {shape} response publishes the frozen version literal"
        );
        assert!(
            response["v"].is_u64(),
            "the version is the integer 1, never the string \"1\": {response}"
        );
    }
}

#[test]
fn every_text_marker_is_emitted_verbatim() {
    let repo = frozen_repo();
    let root = repo.path();
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();

    observe(
        &mut seen,
        &stdout(&run(root, &["query", "verify_token"])),
        &[marker::FINAL_SOURCE_ANCHOR],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "session"])),
        &[marker::CANDIDATES_HEADER],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", UNKNOWN_QUERY])),
        &[marker::NO_ANCHOR_NOT_FOUND],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "verify_token"])),
        &[
            marker::SOURCE_SPAN,
            marker::SOURCE_WINDOW,
            marker::SOURCE_REPRESENTATION,
            marker::SOURCE_COMPLETE,
            marker::SOURCE_FINAL_NEWLINE_PRESENT,
            marker::SOURCE_BYTES,
            marker::SEPARATOR,
        ],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "src/tail.rs"])),
        &[marker::SOURCE_FINAL_NEWLINE_ABSENT],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "clipped_anchor"])),
        &[marker::SOURCE_CONTEXT_CLIPPED],
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "long_anchor"])),
        &[marker::SOURCE_TRUNCATED],
    );

    let weak = low_confidence_repo();
    observe(
        &mut seen,
        &stdout(&run(weak.path(), &["query", WEAK_QUERY])),
        &[marker::NO_ANCHOR_LOW_CONFIDENCE],
    );

    write(
        root,
        "src/auth/token.rs",
        "pub fn verify_token() -> bool { false }\n",
    );
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "verify_token"])),
        &[marker::STALE_SOURCE],
    );

    fs::remove_file(root.join("src/auth/token.rs")).unwrap();
    observe(
        &mut seen,
        &stdout(&run(root, &["query", "--source", "verify_token"])),
        &[marker::SOURCE_REFUSED],
    );

    assert_eq!(
        seen,
        marker::ALL.iter().copied().collect::<BTreeSet<&str>>(),
        "the marker inventory is closed: every marker in it is printed by one of \
         the cases above, and nothing prints a marker that is not in it"
    );
}

#[test]
fn query_exit_codes_are_zero_two_three_four() {
    let repo = frozen_repo();
    let root = repo.path();

    for format in [Vec::new(), vec!["--json"]] {
        for (expected, query) in [(0, "verify_token"), (2, "session"), (3, UNKNOWN_QUERY)] {
            let mut invocation = vec!["query"];
            invocation.extend_from_slice(&format);
            invocation.push(query);
            let output = run(root, &invocation);
            assert_eq!(
                code(&output),
                expected,
                "{invocation:?} must exit {expected}: {}",
                stdout(&output)
            );
        }
    }

    write(
        root,
        "src/auth/token.rs",
        "pub fn verify_token() -> bool { false }\n",
    );
    for format in [Vec::new(), vec!["--json"]] {
        let mut invocation = vec!["query"];
        invocation.extend_from_slice(&format);
        invocation.extend_from_slice(&["--source", "verify_token"]);
        let output = run(root, &invocation);
        assert_eq!(
            code(&output),
            4,
            "a refused source exits 4 whichever format asked for it: {}",
            stdout(&output)
        );
    }
}

#[test]
fn error_exit_is_one_and_never_collides_with_candidates() {
    let repo = frozen_repo();
    let root = repo.path();

    let failed = run(root, &["query", "   "]);
    assert_eq!(code(&failed), 1);
    assert!(
        stdout(&failed).is_empty(),
        "a failure prints no answer at all, so a caller cannot parse one out of it"
    );
    assert!(
        stderr(&failed).starts_with("rr: query: "),
        "a failure names the command that failed: {:?}",
        stderr(&failed)
    );

    let candidates = run(root, &["query", "session"]);
    assert_eq!(code(&candidates), 2);
    assert!(stderr(&candidates).is_empty());
    assert!(
        stdout(&candidates).starts_with(marker::CANDIDATES_HEADER),
        "{:?}",
        stdout(&candidates)
    );

    assert_ne!(
        code(&failed),
        code(&candidates),
        "1 means the question could not be answered and 2 means it was answered \
         with a list; one code for both would make a crash read as a shortlist"
    );
}

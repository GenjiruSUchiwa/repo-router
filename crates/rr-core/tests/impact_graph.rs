//! The impact truth table, one test per row.
//!
//! Every row is a claim about what `rr impact` may and may not say, and each one
//! is checked against a corpus indexed by the shipped builder rather than a graph
//! assembled by hand: a resolved edge is what `index::build` resolves, and a test
//! that invented its own edges would pass while the command reported nothing.
//!
//! Two endpoints per scenario, each its own in-memory snapshot, because that is
//! what the command compares. Nothing here reads the clock, an absolute path or
//! the environment, so a failure is a claim that changed rather than a machine
//! that did.

#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;

use rr_core::facts::DegradedReason;
use rr_core::impact::{
    impact, render_impact_json, render_impact_text, ChangedDefinition, DefinitionChange, Direction,
    Edge, EdgeKind, Endpoint, EndpointJson, FileChange, FileState, Graph, HunkRange, ImpactRequest,
    ImpactResultV1, ImpactStatus, NodeKey, TestReason, DEFAULT_LIMIT, IMPACT_COMMAND,
    IMPACT_CONFLICTED_PATH, IMPACT_SCHEMA_VERSION, IMPACT_WORKTREE_RACED,
};
use rr_core::index::{ContentRepresentation, FileInput, Snapshot, SnapshotBuilder, SnapshotMeta};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::{degraded_facts, Registry};
use rr_core::path::RelPath;

/// `src/auth.rs` before the change: seven lines, two definitions.
const AUTH_BASE: &str = "pub fn find_user(id: u32) -> u32 {
    id
}

pub fn verify_token(token: u32) -> u32 {
    find_user(token)
}
";

/// The same file with line 6 edited.
const AUTH_EDITED: &str = "pub fn find_user(id: u32) -> u32 {
    id
}

pub fn verify_token(token: u32) -> u32 {
    find_user(token) + 1
}
";

/// The same file with `verify_token` gone.
const AUTH_WITHOUT_VERIFY: &str = "pub fn find_user(id: u32) -> u32 {
    id
}

";

/// `src/auth.rs` with a trailing blank line, so an appended definition follows a
/// line that no definition contains.
const AUTH_WITH_ROOM: &str = "pub fn find_user(id: u32) -> u32 {
    id
}

pub fn verify_token(token: u32) -> u32 {
    find_user(token)
}

";

/// The same file with a third definition appended.
const AUTH_WITH_REFRESH: &str = "pub fn find_user(id: u32) -> u32 {
    id
}

pub fn verify_token(token: u32) -> u32 {
    find_user(token)
}

pub fn refresh_token(token: u32) -> u32 {
    find_user(token)
}
";

/// One caller, reaching the definition by its qualified path.
const MAIN: &str = "pub fn main_entry() -> u32 {
    crate::auth::verify_token(1)
}
";

/// One anchor per fixture definition, spelled as `rr query` spells it.
const VERIFY: &str = "src/auth.rs#verify_token";
const FIND_USER: &str = "src/auth.rs#find_user";
const MAIN_ENTRY: &str = "src/main.rs#main_entry";

#[test]
fn impact_row01_edited_definition_reaches_its_caller() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_EDITED), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .run();

    assert_eq!(result.status, ImpactStatus::Complete);
    assert_eq!(result.exit_code(), 0);
    assert_eq!(
        changed(&result),
        vec![(VERIFY.to_owned(), DefinitionChange::Modified)],
        "only the edited definition is a seed"
    );
    assert!(
        result
            .direct_edges
            .iter()
            .any(|edge| edge.source == MAIN_ENTRY
                && edge.target == VERIFY
                && edge.kind == EdgeKind::Call),
        "the caller's edge is a direct edge: {:?}",
        result.direct_edges
    );
    assert_eq!(at(&result.affected, MAIN_ENTRY), Some(1));
    assert_eq!(at(&result.dependencies, FIND_USER), Some(1));
    assert!(
        result.resolution.exact_edges >= 2,
        "both incident edges are exact: {:?}",
        result.resolution
    );
}

#[test]
fn impact_row02_deleted_definition_keeps_its_base_side_edge() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_WITHOUT_VERIFY), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_WITHOUT_VERIFY,
        &[hunk(5, 3, 4, 0)],
    ))
    .run();

    assert_eq!(
        changed(&result),
        vec![(VERIFY.to_owned(), DefinitionChange::Removed)],
        "the definition the target no longer declares is removed"
    );
    let edge = result
        .direct_edges
        .iter()
        .find(|edge| edge.source == MAIN_ENTRY && edge.target == VERIFY)
        .expect("the base endpoint still holds the caller's edge");
    assert_eq!(
        edge.endpoint,
        Endpoint::Base,
        "an edge only the base endpoint resolves is a base-side edge"
    );
    assert_eq!(at(&result.affected, MAIN_ENTRY), Some(1));
}

#[test]
fn impact_row03_added_definition_has_no_callers_and_keeps_its_callees() {
    let result = scenario(
        &[("src/auth.rs", AUTH_WITH_ROOM), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_WITH_REFRESH), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_WITH_ROOM,
        AUTH_WITH_REFRESH,
        &[hunk(8, 0, 9, 3)],
    ))
    .run();

    assert_eq!(
        changed(&result),
        vec![(
            "src/auth.rs#refresh_token".to_owned(),
            DefinitionChange::Added
        )],
        "only the new definition is a seed"
    );
    assert!(
        result.affected.is_empty(),
        "nothing points at a definition that did not exist: {:?}",
        result.affected
    );
    assert_eq!(at(&result.dependencies, FIND_USER), Some(1));
    assert!(
        !result
            .direct_edges
            .iter()
            .any(|edge| edge.target == "src/auth.rs#refresh_token"),
        "no edge points at the new definition: {:?}",
        result.direct_edges
    );
}

/// A pure move reaches nothing new.
///
/// The truth table's "none" is read as *nothing outside the change*: the two
/// moved definitions call each other, so each is reached from the other, and both
/// are seeds — which under D2 is distance `0`. What must not appear is a node the
/// move did not touch.
#[test]
fn impact_row04_rename_with_identical_bytes_reports_moved() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE)],
        &[("src/token.rs", AUTH_BASE)],
    )
    .change(FileChange {
        path: RelPath::new("src/token.rs").unwrap(),
        source: Some(RelPath::new("src/auth.rs").unwrap()),
        base_oid: Some(oid_of(AUTH_BASE)),
        target_oid: Some(oid_of(AUTH_BASE)),
        hunks: Vec::new(),
    })
    .run();

    assert_eq!(
        changed(&result),
        vec![
            ("src/token.rs#find_user".to_owned(), DefinitionChange::Moved),
            (
                "src/token.rs#verify_token".to_owned(),
                DefinitionChange::Moved
            ),
        ],
        "bytes that did not change moved rather than changed"
    );
    assert!(
        result
            .affected
            .iter()
            .chain(&result.dependencies)
            .all(|node| node.distance == 0 && node.path == "src/token.rs"),
        "a move reaches nothing the move did not touch: {:?} {:?}",
        result.affected,
        result.dependencies
    );
}

#[test]
fn impact_row05_edited_method_call_site_resolves_to_nothing() {
    let result = method_call_scenario();

    assert!(
        result.resolution.unresolved_calls >= 1,
        "the method call is counted: {:?}",
        result.resolution
    );
    assert!(
        !result
            .direct_edges
            .iter()
            .any(|edge| edge.target == "src/thing.rs#value"),
        "no edge is invented for a receiver rr cannot type: {:?}",
        result.direct_edges
    );
}

#[test]
fn impact_row06_typescript_import_is_counted_and_listed() {
    let result = typescript_scenario();

    assert_eq!(result.resolution.unresolved_imports, 1);
    assert_eq!(result.unfollowed_imports.len(), 1);
    assert!(
        result.affected.is_empty(),
        "an import nothing can follow reaches nothing: {:?}",
        result.affected
    );
}

#[test]
fn impact_row07_two_definitions_sharing_a_name_are_ambiguous() {
    let base = "pub trait Marker {}
";
    let other = "pub trait Marker {}
";
    let user_base = "pub struct Thing;

impl Marker for Thing {}
";
    let user_target = "pub struct Thing;

impl Marker for Thing {}

pub fn touch() -> u32 {
    1
}
";
    let result = scenario(
        &[
            ("src/a/mod.rs", base),
            ("src/a/lib.rs", other),
            ("src/a/main.rs", user_base),
        ],
        &[
            ("src/a/mod.rs", base),
            ("src/a/lib.rs", other),
            ("src/a/main.rs", user_target),
        ],
    )
    .change(modified(
        "src/a/main.rs",
        user_base,
        user_target,
        &[hunk(3, 1, 3, 5)],
    ))
    .run();

    assert!(
        result.resolution.ambiguous_references >= 1,
        "a name two definitions answer to is ambiguous, never an edge: {:?}",
        result.resolution
    );
    assert!(
        !result
            .direct_edges
            .iter()
            .any(|edge| edge.target.ends_with("#Marker")),
        "an ambiguous reference produces no edge: {:?}",
        result.direct_edges
    );
}

#[test]
fn impact_row08_mutual_recursion_between_seeds_is_one_cycle() {
    let base = "pub fn ping(n: u32) -> u32 {
    pong(n)
}

pub fn pong(n: u32) -> u32 {
    ping(n)
}
";
    let target = "pub fn ping(n: u32) -> u32 {
    pong(n) + 1
}

pub fn pong(n: u32) -> u32 {
    ping(n) + 1
}
";
    let result = scenario(&[("src/ring.rs", base)], &[("src/ring.rs", target)])
        .change(rewritten("src/ring.rs", base, target))
        .run();

    assert!(
        result
            .direct_edges
            .iter()
            .any(|edge| edge.source == "src/ring.rs#ping" && edge.target == "src/ring.rs#pong")
            && result
                .direct_edges
                .iter()
                .any(|edge| edge.source == "src/ring.rs#pong" && edge.target == "src/ring.rs#ping"),
        "both edges are incident to a seed: {:?}",
        result.direct_edges
    );
    assert!(at(&result.affected, "src/ring.rs#ping").is_some());
    assert!(at(&result.affected, "src/ring.rs#pong").is_some());
    assert_eq!(result.cycles.len(), 1, "one component: {:?}", result.cycles);
    assert_eq!(
        result.cycles[0].members,
        vec!["src/ring.rs#ping".to_owned(), "src/ring.rs#pong".to_owned()],
        "both members, sorted by anchor"
    );
}

#[test]
fn impact_row09_self_recursion_is_one_self_loop() {
    let base = "pub fn spin(n: u32) -> u32 {
    spin(n)
}
";
    let target = "pub fn spin(n: u32) -> u32 {
    spin(n) + 1
}
";
    let result = scenario(&[("src/spin.rs", base)], &[("src/spin.rs", target)])
        .change(modified("src/spin.rs", base, target, &[hunk(2, 1, 2, 1)]))
        .run();

    assert!(
        result
            .direct_edges
            .iter()
            .any(|edge| edge.source == "src/spin.rs#spin" && edge.target == "src/spin.rs#spin"),
        "the self edge is a direct edge: {:?}",
        result.direct_edges
    );
    assert_eq!(
        at(&result.affected, "src/spin.rs#spin"),
        Some(0),
        "a seed is at distance zero even when a chain returns to it"
    );
    assert_eq!(at(&result.dependencies, "src/spin.rs#spin"), Some(0));
    assert_eq!(result.cycles.len(), 1, "one self-loop: {:?}", result.cycles);
    assert_eq!(
        result.cycles[0].members,
        vec!["src/spin.rs#spin".to_owned()]
    );
}

#[test]
fn impact_row10_unparsable_target_file_is_listed_and_counted() {
    let base = "pub fn parses() -> u32 {
    1
}
";
    let target = "pub fn parses( -> {{{
";
    let result = Scenario {
        base: corpus(&[("src/broken.rs", base)]),
        target: degraded_corpus(&[("src/broken.rs", target)]),
        ..Scenario::empty()
    }
    .change(rewritten("src/broken.rs", base, target))
    .run();

    assert_eq!(result.changed_files.len(), 1);
    assert_eq!(result.changed_files[0].state, FileState::Degraded);
    assert_eq!(result.resolution.degraded_files, 1);
    assert!(
        result.affected.is_empty() && result.dependencies.is_empty(),
        "a file rr could not parse yields no closure"
    );
}

#[test]
fn impact_row11_raced_path_is_excluded_and_the_report_is_partial() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_EDITED), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .raced("src/auth.rs")
    .run();

    assert_eq!(result.status, ImpactStatus::Partial);
    assert_eq!(result.exit_code(), 1);
    assert!(
        result.changed_files.is_empty() && result.changed_definitions.is_empty(),
        "a path whose bytes moved is excluded rather than guessed at"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == IMPACT_WORKTREE_RACED),
        "the report says why it is partial: {:?}",
        result.diagnostics
    );
}

#[test]
fn impact_row12_conflicted_path_is_listed_and_diagnosed() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE)],
        &[("src/auth.rs", AUTH_EDITED)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .conflicted("src/auth.rs")
    .run();

    assert_eq!(result.changed_files.len(), 1);
    assert_eq!(result.changed_files[0].state, FileState::Conflicted);
    assert_eq!(result.changed_files[0].definitions, 0);
    assert!(
        result.changed_definitions.is_empty(),
        "a path with two base sides is compared against neither"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == IMPACT_CONFLICTED_PATH),
        "the report names the conflict: {:?}",
        result.diagnostics
    );
    assert_eq!(
        result.status,
        ImpactStatus::Complete,
        "a conflict is a repository state, not a failure to evaluate"
    );
}

#[test]
fn impact_row13_depth_zero_reports_edges_and_no_closure() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_EDITED), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .depth(0)
    .run();

    assert!(
        !result.direct_edges.is_empty(),
        "the incident edges are still reported"
    );
    assert!(
        result.affected.is_empty() && result.dependencies.is_empty(),
        "no closure is walked at depth zero"
    );
    assert_eq!(result.depth, 0);
}

#[test]
fn impact_row14_limit_reports_shown_over_total() {
    let mut callers = String::new();
    for index in 0..9 {
        let _ = writeln!(
            callers,
            "pub fn caller_{index}() -> u32 {{
    crate::auth::verify_token({index})
}}
"
        );
    }
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/callers.rs", &callers)],
        &[("src/auth.rs", AUTH_EDITED), ("src/callers.rs", &callers)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .run();

    assert_eq!(
        result.affected.len(),
        9,
        "nine callers: {:?}",
        result.affected
    );
    let text = render_impact_text(&result, 1);
    assert!(
        text.contains("affected (shown 1/9):"),
        "the text form says what it did not print:\n{text}"
    );
    let json = render_impact_json(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed["affected"].as_array().map(Vec::len),
        Some(9),
        "the JSON object is never truncated"
    );
    assert_eq!(parsed["schema_version"], IMPACT_SCHEMA_VERSION);
    assert_eq!(parsed["command"], IMPACT_COMMAND);
}

#[test]
fn node_key_matches_the_same_definition_across_endpoints() {
    let result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_EDITED), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .run();

    let matching: Vec<&rr_core::impact::ImpactEdge> = result
        .direct_edges
        .iter()
        .filter(|edge| edge.source == MAIN_ENTRY && edge.target == VERIFY)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "one edge, not one per endpoint: {matching:?}"
    );
    assert_eq!(
        matching[0].endpoint,
        Endpoint::Both,
        "two arenas numbered it differently and it is still one definition"
    );
}

#[test]
fn unresolved_method_call_is_counted_never_an_edge() {
    let result = method_call_scenario();

    assert!(
        result.resolution.unresolved_calls >= 1,
        "the observation is reported: {:?}",
        result.resolution
    );
    assert!(
        result
            .direct_edges
            .iter()
            .all(|edge| edge.target != "src/thing.rs#value"),
        "a definition of that name exists and is still not an edge: {:?}",
        result.direct_edges
    );
    assert!(
        result
            .affected
            .iter()
            .all(|node| node.anchor != "src/thing.rs#value"),
        "and it reaches nothing: {:?}",
        result.affected
    );
}

#[test]
fn unfollowed_import_records_specifier_and_leaf_separately() {
    let result = typescript_scenario();

    let import = &result.unfollowed_imports[0];
    assert_eq!(import.path, "src/app.ts");
    assert_eq!(import.specifier, "./auth");
    assert_eq!(import.name.as_deref(), Some("verifyToken"));
    assert_eq!(import.why, "unresolved-by-design");
    assert!(
        !import.specifier.contains("verifyToken"),
        "the specifier is never a path spelled out of both halves"
    );
    let text = render_impact_text(&result, DEFAULT_LIMIT);
    assert!(
        text.contains("note: 1 import(s) in changed files cannot be followed"),
        "the count is stated so an empty closure cannot be misread:\n{text}"
    );
}

#[test]
fn zero_width_insertion_belongs_to_the_innermost_definition() {
    let base = "pub struct Thing;

impl Thing {
    pub fn first(&self) -> u32 {
        1
    }

    pub fn second(&self) -> u32 {
        2
    }
}
";
    let target = "pub struct Thing;

impl Thing {
    pub fn first(&self) -> u32 {
        0;
        1
    }

    pub fn second(&self) -> u32 {
        2
    }
}
";
    let result = scenario(&[("src/thing.rs", base)], &[("src/thing.rs", target)])
        .change(modified("src/thing.rs", base, target, &[hunk(4, 0, 5, 1)]))
        .run();

    let anchors: Vec<String> = result
        .changed_definitions
        .iter()
        .map(|definition| definition.anchor.clone())
        .collect();
    assert_eq!(
        anchors,
        vec!["src/thing.rs#first".to_owned()],
        "the method gained a line; the block around it did not change"
    );
}

#[test]
fn rename_pairs_definitions_by_kind_and_qualified_name() {
    let base = "pub fn alpha() -> u32 {
    1
}

pub fn beta() -> u32 {
    2
}
";
    let target = "pub fn alpha() -> u32 {
    1
}

pub fn gamma() -> u32 {
    2
}
";
    let result = scenario(&[("src/a/mod.rs", base)], &[("src/a/lib.rs", target)])
        .change(FileChange {
            path: RelPath::new("src/a/lib.rs").unwrap(),
            source: Some(RelPath::new("src/a/mod.rs").unwrap()),
            base_oid: Some(oid_of(base)),
            target_oid: Some(oid_of(target)),
            hunks: vec![hunk(5, 1, 5, 1)],
        })
        .run();

    assert_eq!(
        changed(&result),
        vec![
            ("src/a/lib.rs#alpha".to_owned(), DefinitionChange::Moved),
            ("src/a/lib.rs#gamma".to_owned(), DefinitionChange::Added),
            ("src/a/mod.rs#beta".to_owned(), DefinitionChange::Removed),
        ],
        "the name that paired moved; the two that did not are one removal and \
         one addition, never one fuzzy match"
    );
}

#[test]
fn iterative_tarjan_survives_a_ten_thousand_node_chain() {
    const LENGTH: usize = 10_000;

    let mut graph = Graph::new();
    let nodes: Vec<NodeKey> = (0..LENGTH)
        .map(|index| NodeKey::definition("src/chain.rs", &format!("chain::link{index:05}")))
        .collect();
    for window in nodes.windows(2) {
        graph.insert(Edge {
            source: window[0].clone(),
            target: window[1].clone(),
            kind: EdgeKind::Call,
            line: 1,
            endpoint: Endpoint::Target,
        });
    }
    let every: std::collections::BTreeSet<NodeKey> = nodes.iter().cloned().collect();

    assert_eq!(graph.len(), LENGTH - 1);
    assert!(
        graph.cycles(&every).is_empty(),
        "a chain is not a cycle, and finding that out must not overflow the stack"
    );
    let seeds: std::collections::BTreeSet<NodeKey> = std::iter::once(nodes[0].clone()).collect();
    assert_eq!(
        graph.traverse(&seeds, Direction::Outgoing, 8).len(),
        8,
        "a bounded closure walks the bound and stops"
    );
}

#[test]
fn canonicalize_is_idempotent() {
    let mut result = scenario(
        &[("src/auth.rs", AUTH_BASE), ("src/main.rs", MAIN)],
        &[("src/auth.rs", AUTH_EDITED), ("src/main.rs", MAIN)],
    )
    .change(modified(
        "src/auth.rs",
        AUTH_BASE,
        AUTH_EDITED,
        &[hunk(6, 1, 6, 1)],
    ))
    .run();

    let once = result.clone();
    result.canonicalize();
    assert_eq!(
        result, once,
        "the order impact publishes is already settled"
    );
    assert_eq!(
        render_impact_text(&result, DEFAULT_LIMIT),
        render_impact_text(&once, DEFAULT_LIMIT),
        "and both renderers see it"
    );
}

#[test]
fn overlay_never_writes_the_snapshot_file() {
    let root = tempfile::tempdir().unwrap();
    let published = corpus(&[("src/auth.rs", AUTH_BASE)]);
    let snapshot_path = rr_core::workspace::snapshot_path(root.path());

    let overlaid = rr_core::impact::overlay(&published, inputs(&[("src/auth.rs", AUTH_EDITED)]))
        .expect("build an overlay");

    assert_eq!(overlaid.files.len(), 1);
    assert!(
        !snapshot_path.exists(),
        "an overlay describes one side of one comparison and is never published"
    );
    assert!(
        !root.path().join(".rr").exists(),
        "and it creates nothing on the way"
    );
}

#[test]
fn co_change_evidence_is_probable_and_never_affects() {
    let base = "pub fn find_user(id: u32) -> u32 {
    id
}
";
    let target = "pub fn find_user(id: u32) -> u32 {
    id + 1
}
";
    let test_file = "#[test]
fn checks_nothing() {
    assert!(true);
}
";
    let result = Scenario {
        base: corpus(&[("src/auth.rs", base), ("tests/auth_test.rs", test_file)]),
        target: corpus(&[("src/auth.rs", target), ("tests/auth_test.rs", test_file)]),
        ..Scenario::empty()
    }
    .change(modified("src/auth.rs", base, target, &[hunk(2, 1, 2, 1)]))
    .co_changed("tests/auth_test.rs")
    .run();

    let test = result
        .tests
        .iter()
        .find(|test| test.path == "tests/auth_test.rs")
        .expect("the co-changed test file is named");
    assert!(test.reasons.contains(&TestReason::CoChange));
    assert_eq!(test.confidence, rr_core::impact::Evidence::Probable);
    assert!(
        result
            .affected
            .iter()
            .all(|node| node.path != "tests/auth_test.rs"),
        "a correlation names a test and never enters the closure: {:?}",
        result.affected
    );
}

/// The `obj.method()` corpus, where a definition of that name also exists.
///
/// The definition is what makes the test mean something: a resolver that matched
/// names would produce an edge here, and the counter is the honest answer instead.
fn method_call_scenario() -> ImpactResultV1 {
    let thing = "pub struct Thing;

impl Thing {
    pub fn value(&self) -> u32 {
        1
    }
}
";
    let base = "pub fn run(thing: crate::thing::Thing) -> u32 {
    thing.value()
}
";
    let target = "pub fn run(thing: crate::thing::Thing) -> u32 {
    thing.value() + 1
}
";
    scenario(
        &[("src/thing.rs", thing), ("src/run.rs", base)],
        &[("src/thing.rs", thing), ("src/run.rs", target)],
    )
    .change(modified("src/run.rs", base, target, &[hunk(2, 1, 2, 1)]))
    .run()
}

/// The TypeScript corpus, whose imports resolve by name and not by path.
fn typescript_scenario() -> ImpactResultV1 {
    let auth = "export function verifyToken(): number {
  return 1;
}
";
    let base = "import { verifyToken } from \"./auth\";

export function handle(): number {
  return 0;
}
";
    let target = "import { verifyToken } from \"./auth\";

export function handle(): number {
  return verifyToken();
}
";
    scenario(
        &[("src/auth.ts", auth), ("src/app.ts", base)],
        &[("src/auth.ts", auth), ("src/app.ts", target)],
    )
    .change(modified("src/app.ts", base, target, &[hunk(4, 1, 4, 1)]))
    .run()
}

/// One comparison, assembled from two corpora.
struct Scenario {
    base: Snapshot,
    target: Snapshot,
    changes: Vec<FileChange>,
    raced: Vec<RelPath>,
    conflicted: Vec<RelPath>,
    co_changed: Vec<RelPath>,
    depth: u8,
}

impl Scenario {
    /// A comparison of two empty corpora, for a test that fills in one side.
    fn empty() -> Self {
        Self {
            base: corpus(&[]),
            target: corpus(&[]),
            changes: Vec::new(),
            raced: Vec::new(),
            conflicted: Vec::new(),
            co_changed: Vec::new(),
            depth: 2,
        }
    }

    fn change(mut self, change: FileChange) -> Self {
        self.changes.push(change);
        self
    }

    fn raced(mut self, path: &str) -> Self {
        self.raced.push(RelPath::new(path).unwrap());
        self
    }

    fn conflicted(mut self, path: &str) -> Self {
        self.conflicted.push(RelPath::new(path).unwrap());
        self
    }

    fn co_changed(mut self, path: &str) -> Self {
        self.co_changed.push(RelPath::new(path).unwrap());
        self
    }

    fn depth(mut self, depth: u8) -> Self {
        self.depth = depth;
        self
    }

    fn run(&self) -> ImpactResultV1 {
        impact(&ImpactRequest {
            base: EndpointJson::tree("HEAD", &oid_of("HEAD")),
            target: EndpointJson::worktree(Some(&oid_of("HEAD"))),
            base_snapshot: &self.base,
            target_snapshot: &self.target,
            changes: &self.changes,
            raced: &self.raced,
            conflicted: &self.conflicted,
            co_changed: &self.co_changed,
            depth: self.depth,
        })
        .expect("evaluate one comparison")
    }
}

/// A comparison of two corpora given as sources.
fn scenario(base: &[(&str, &str)], target: &[(&str, &str)]) -> Scenario {
    Scenario {
        base: corpus(base),
        target: corpus(target),
        ..Scenario::empty()
    }
}

/// Indexes sources held in memory, each in the language its extension names.
fn corpus(sources: &[(&str, &str)]) -> Snapshot {
    build(inputs(sources))
}

/// Indexes sources whose extractor is not asked, as an unparsable file is.
fn degraded_corpus(sources: &[(&str, &str)]) -> Snapshot {
    let prepared = sources
        .iter()
        .map(|(path, code)| {
            let facts = degraded_facts(code.as_bytes(), DegradedReason::ParserReturnedNone);
            FileInput {
                path: RelPath::new(*path).unwrap(),
                oid: oid_of(code),
                representation: ContentRepresentation::RawNoGit,
                generated: false,
                language: lang_of(path),
                parse_status: facts.status(),
                facts,
            }
        })
        .collect();
    build(prepared)
}

/// Extracts facts for each source, in the shipped extractor for its language.
fn inputs(sources: &[(&str, &str)]) -> Vec<FileInput> {
    let mut registry = Registry::new();
    sources
        .iter()
        .map(|(path, code)| {
            let language = lang_of(path);
            let extractor = registry
                .for_lang(language)
                .expect("a fixture language has an extractor")
                .expect("the extractor builds");
            let facts = extractor
                .extract(code.as_bytes())
                .expect("extract fixture facts");
            FileInput {
                path: RelPath::new(*path).unwrap(),
                oid: oid_of(code),
                representation: ContentRepresentation::RawNoGit,
                generated: false,
                language,
                parse_status: facts.status(),
                facts,
            }
        })
        .collect()
}

fn build(inputs: Vec<FileInput>) -> Snapshot {
    let (snapshot, _counts) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
        .build(inputs)
        .expect("index a fixture corpus");
    snapshot
}

fn lang_of(path: &str) -> Lang {
    let extension = path
        .rsplit_once('.')
        .expect("a fixture path has an extension");
    Lang::from_extension(extension.1).expect("a fixture extension names a language")
}

/// The content identity of one source, as the fixture builder records it.
fn oid_of(code: &str) -> Oid {
    Oid::from_raw(blake3::hash(code.as_bytes()).as_bytes()).expect("hash a fixture source")
}

fn hunk(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> HunkRange {
    HunkRange {
        old_start,
        old_lines,
        new_start,
        new_lines,
    }
}

fn modified(path: &str, base: &str, target: &str, hunks: &[HunkRange]) -> FileChange {
    FileChange {
        path: RelPath::new(path).unwrap(),
        source: None,
        base_oid: Some(oid_of(base)),
        target_oid: Some(oid_of(target)),
        hunks: hunks.to_vec(),
    }
}

/// A change no line diff addresses, which means the whole file changed.
fn rewritten(path: &str, base: &str, target: &str) -> FileChange {
    modified(path, base, target, &[])
}

/// Every changed definition, as `(anchor, change)` in report order.
fn changed(result: &ImpactResultV1) -> Vec<(String, DefinitionChange)> {
    result
        .changed_definitions
        .iter()
        .map(|definition: &ChangedDefinition| (definition.anchor.clone(), definition.change))
        .collect()
}

/// The distance one anchor was reached at, if it was reached.
fn at(nodes: &[rr_core::impact::ImpactNode], anchor: &str) -> Option<u8> {
    nodes
        .iter()
        .find(|node| node.anchor == anchor)
        .map(|node| node.distance)
}

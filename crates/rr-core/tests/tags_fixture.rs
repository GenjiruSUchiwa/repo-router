#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use rr_core::facts::display_signature;
use rr_core::parser::Registry;
use rr_core::refresh::{render_refresh_json, RefreshCommand, RefreshReport};
use rr_core::{Facts, Lang, ParseStatus};
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotFile {
    path: String,
    status: String,
    defs: Vec<SnapshotDef>,
    references: Vec<SnapshotRef>,
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotDef {
    name: String,
    local_qualified: Option<String>,
    kind: String,
    visibility: String,
    start_line: u32,
    end_line: u32,
    signature: String,
    signature_idents: Vec<String>,
    body_idents: Vec<String>,
    doc_idents: Vec<String>,
    attribute_idents: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotRef {
    name: String,
    line: u32,
    owner: Option<String>,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/python")
        .join(name)
}

fn extract_python(content: &[u8]) -> Facts {
    let mut registry = Registry::new();
    registry
        .for_lang(Lang::Python)
        .unwrap()
        .unwrap()
        .extract(content)
        .unwrap()
}

fn status_label(status: ParseStatus) -> String {
    match status {
        ParseStatus::Complete => "complete".to_owned(),
        ParseStatus::Recovered {
            error_nodes,
            missing_nodes,
        } => format!("recovered(error={error_nodes},missing={missing_nodes})"),
        ParseStatus::Tags { parse_errors } => format!("tags(parse_errors={parse_errors})"),
        ParseStatus::Degraded {
            reason,
            scanned_bytes,
            truncated,
        } => format!("degraded({reason:?},scanned={scanned_bytes},truncated={truncated})"),
    }
}

fn owner_name(facts: &Facts, owner: Option<rr_core::LocalDefId>) -> Option<String> {
    owner.and_then(|id| facts.def(id).map(|def| def.name.clone()))
}

fn to_snapshot(path: &str, facts: &Facts) -> SnapshotFile {
    SnapshotFile {
        path: path.to_owned(),
        status: status_label(facts.status()),
        defs: facts
            .defs()
            .iter()
            .map(|def| SnapshotDef {
                name: def.name.clone(),
                local_qualified: def.local_qualified.clone(),
                kind: def.kind.to_string(),
                visibility: format!("{:?}", def.visibility).to_lowercase(),
                start_line: def.span.start_line(),
                end_line: def.span.end_line(),
                signature: def.signature.clone(),
                signature_idents: def.signature_idents.clone(),
                body_idents: def.body_idents.clone(),
                doc_idents: def.doc_idents.clone(),
                attribute_idents: def.attribute_idents.clone(),
            })
            .collect(),
        references: facts
            .references()
            .iter()
            .map(|reference| SnapshotRef {
                name: reference.name.clone(),
                line: reference.span.start_line(),
                owner: owner_name(facts, reference.owner),
            })
            .collect(),
    }
}

#[test]
fn a_tags_extractor_produces_the_same_facts_on_every_run() {
    let content = fs::read(fixture("surface.py")).unwrap();
    let mut reusable = Registry::new();
    let extractor = reusable.for_lang(Lang::Python).unwrap().unwrap();
    let first = extractor.extract(&content).unwrap();
    let second = extractor.extract(&content).unwrap();
    let fresh = extract_python(&content);
    assert_eq!(first, second);
    assert_eq!(first, fresh);

    let bytes = postcard::to_allocvec(&first).unwrap();
    let round_trip: Facts = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(round_trip, first);
}

#[test]
fn a_tags_based_file_is_reported_at_its_real_fidelity() {
    let content = fs::read(fixture("surface.py")).unwrap();
    let facts = extract_python(&content);
    assert!(matches!(facts.status(), ParseStatus::Tags { .. }));
    assert!(!matches!(facts.status(), ParseStatus::Complete));

    let report = RefreshReport {
        tags: 1,
        ..RefreshReport::default()
    };
    let json = render_refresh_json(&report, RefreshCommand::Refresh).unwrap();
    assert!(json.contains(r#""schema_version":3"#));
    assert!(json.contains(r#""tags":1"#));
}

#[test]
fn a_tags_fixture_has_a_readable_golden_projection() {
    let content = fs::read(fixture("surface.py")).unwrap();
    let facts = extract_python(&content);
    insta::assert_yaml_snapshot!("python_surface", to_snapshot("surface.py", &facts));
}

#[test]
fn a_recovered_tags_fixture_records_parse_errors() {
    let content = fs::read(fixture("recovered.py")).unwrap();
    let facts = extract_python(&content);
    assert!(matches!(
        facts.status(),
        ParseStatus::Tags { parse_errors: true }
    ));
    assert!(facts.defs().iter().any(|def| def.name == "before"));
    assert!(facts.defs().iter().any(|def| def.name == "after"));
    insta::assert_yaml_snapshot!("python_recovered", to_snapshot("recovered.py", &facts));
}

#[test]
fn signature_text_is_sliced_from_the_span_not_reconstructed() {
    let content = fs::read(fixture("surface.py")).unwrap();
    let source = std::str::from_utf8(&content).unwrap();
    let facts = extract_python(&content);
    for def in facts.defs() {
        let raw = &source
            [def.signature_span.start_byte() as usize..def.signature_span.end_byte() as usize];
        assert_eq!(def.signature, display_signature(raw));
    }
}

#[test]
fn tags_fixture_carries_nesting_and_body_identifier_invariants() {
    let content = fs::read(fixture("surface.py")).unwrap();
    let facts = extract_python(&content);
    let method = facts.defs().iter().find(|def| def.name == "run").unwrap();
    assert_eq!(method.local_qualified.as_deref(), Some("Service.run"));
    assert!(method.body_idents.iter().any(|ident| ident == "helper"));
    assert!(!method.body_idents.iter().any(|ident| ident == "hidden"));
    assert!(facts
        .references()
        .iter()
        .any(|reference| reference.name == "helper" && reference.owner.is_some()));
}

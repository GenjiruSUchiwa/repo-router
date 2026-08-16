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

fn fixture(language: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(language)
        .join(name)
}

fn extract(lang: Lang, content: &[u8]) -> Facts {
    let mut registry = Registry::new();
    registry
        .for_lang(lang)
        .unwrap()
        .unwrap()
        .extract(content)
        .unwrap()
}

/// Reads one fixture and extracts it, so a test names a file and a language
/// rather than repeating the two steps between them.
fn facts_for(lang: Lang, language: &str, name: &str) -> (Vec<u8>, Facts) {
    let content = fs::read(fixture(language, name)).unwrap();
    let facts = extract(lang, &content);
    (content, facts)
}

fn def<'a>(facts: &'a Facts, name: &str) -> &'a rr_core::facts::Def {
    facts
        .defs()
        .iter()
        .find(|def| def.name == name)
        .unwrap_or_else(|| panic!("no definition named {name}"))
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
    let content = fs::read(fixture("python", "surface.py")).unwrap();
    let mut reusable = Registry::new();
    let extractor = reusable.for_lang(Lang::Python).unwrap().unwrap();
    let first = extractor.extract(&content).unwrap();
    let second = extractor.extract(&content).unwrap();
    let fresh = extract(Lang::Python, &content);
    assert_eq!(first, second);
    assert_eq!(first, fresh);

    let bytes = postcard::to_allocvec(&first).unwrap();
    let round_trip: Facts = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(round_trip, first);
}

#[test]
fn a_tags_based_file_is_reported_at_its_real_fidelity() {
    let content = fs::read(fixture("python", "surface.py")).unwrap();
    let facts = extract(Lang::Python, &content);
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
    let content = fs::read(fixture("python", "surface.py")).unwrap();
    let facts = extract(Lang::Python, &content);
    insta::assert_yaml_snapshot!("python_surface", to_snapshot("surface.py", &facts));
}

#[test]
fn a_recovered_tags_fixture_records_parse_errors() {
    let content = fs::read(fixture("python", "recovered.py")).unwrap();
    let facts = extract(Lang::Python, &content);
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
    let content = fs::read(fixture("python", "surface.py")).unwrap();
    let source = std::str::from_utf8(&content).unwrap();
    let facts = extract(Lang::Python, &content);
    for def in facts.defs() {
        let raw = &source
            [def.signature_span.start_byte() as usize..def.signature_span.end_byte() as usize];
        assert_eq!(def.signature, display_signature(raw));
    }
}

#[test]
fn tags_fixture_carries_nesting_and_body_identifier_invariants() {
    let content = fs::read(fixture("python", "surface.py")).unwrap();
    let facts = extract(Lang::Python, &content);
    let method = facts.defs().iter().find(|def| def.name == "run").unwrap();
    assert_eq!(method.local_qualified.as_deref(), Some("Service.run"));
    assert!(method.body_idents.iter().any(|ident| ident == "helper"));
    assert!(!method.body_idents.iter().any(|ident| ident == "hidden"));
    assert!(facts
        .references()
        .iter()
        .any(|reference| reference.name == "helper" && reference.owner.is_some()));
}

/// A dedent the grammar has to recover from, which is a different failure to
/// `recovered.py`'s unclosed parenthesis: one breaks the token stream, the
/// other breaks the block structure an indentation-sensitive grammar builds
/// its tree out of. Neither may cost the definitions on the far side.
///
/// The two are not reported alike, and this is the test that says so. An
/// unclosed parenthesis reaches the tree as an error node and `recovered.py`
/// is labelled `parse_errors: true`. Broken indentation never does: Python's
/// grammar resolves columns into indent and dedent tokens in its external
/// scanner, so a dedent that fits no open block is *consumed* there and the
/// tree that comes back is well formed and differently shaped. Nothing
/// downstream can tell it apart from a file that meant what it said, which is
/// why the assertions below are about the definitions rather than the status.
#[test]
fn a_broken_python_indent_recovers_rather_than_truncating() {
    let (_, facts) = facts_for(Lang::Python, "python", "indent.py");
    assert!(matches!(
        facts.status(),
        ParseStatus::Tags {
            parse_errors: false
        }
    ));

    // Every definition survives, on both sides of the break and inside it.
    assert_eq!(def(&facts, "before").kind.to_string(), "function");
    assert_eq!(def(&facts, "Service").kind.to_string(), "class");
    assert_eq!(
        def(&facts, "run").local_qualified.as_deref(),
        Some("Service.run")
    );
    assert_eq!(def(&facts, "after").kind.to_string(), "function");

    // And the reshaping is visible rather than papered over. The dedented
    // `return result` leaves the method the file wrote it inside and lands on
    // the class the grammar hung it on; what the method keeps is the line
    // above it, which was indented correctly.
    let method = def(&facts, "run");
    let class = def(&facts, "Service");
    assert!(method.body_idents.iter().any(|ident| ident == "helper"));
    assert!(!method.body_idents.iter().any(|ident| ident == "return"));
    assert!(class.body_idents.iter().any(|ident| ident == "return"));

    insta::assert_yaml_snapshot!("python_indent", to_snapshot("indent.py", &facts));
}

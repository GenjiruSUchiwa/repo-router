#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use rr_core::facts::display_signature;
use rr_core::parser::Registry;
use rr_core::refresh::{
    render_refresh_json, RefreshCommand, RefreshReport, ReportDetail, RunReport,
};
use rr_core::{Facts, Lang, ParseStatus, Visibility};
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotFile {
    path: String,
    status: String,
    defs: Vec<SnapshotDef>,
    references: Vec<SnapshotRef>,
    imports: Vec<SnapshotImport>,
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotImport {
    kind: String,
    path: String,
    name: Option<String>,
    alias: Option<String>,
    is_public: bool,
    is_glob: bool,
    line: u32,
    owner: Option<String>,
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
    kind: String,
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
                kind: format!("{:?}", reference.kind).to_lowercase(),
                line: reference.span.start_line(),
                owner: owner_name(facts, reference.owner),
            })
            .collect(),
        imports: facts
            .imports()
            .iter()
            .map(|import| SnapshotImport {
                kind: format!("{:?}", import.kind),
                path: import.path.clone(),
                name: import.name.clone(),
                alias: import.alias.clone(),
                is_public: import.is_public,
                is_glob: import.is_glob,
                line: import.span.start_line(),
                owner: owner_name(facts, import.owner),
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

    let report = RunReport {
        refresh: RefreshReport {
            tags: 1,
            ..RefreshReport::default()
        },
        ..RunReport::default()
    };
    let json =
        render_refresh_json(&report, RefreshCommand::Refresh, ReportDetail::Summary).unwrap();
    assert!(json.contains(r#""schema_version":4"#));
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

#[test]
fn a_typescript_extractor_produces_the_same_facts_on_every_run() {
    let content = fs::read(fixture("typescript", "surface.ts")).unwrap();
    let mut reusable = Registry::new();
    let extractor = reusable.for_lang(Lang::TypeScript).unwrap().unwrap();
    let first = extractor.extract(&content).unwrap();
    let second = extractor.extract(&content).unwrap();
    assert_eq!(first, second);
    assert_eq!(first, extract(Lang::TypeScript, &content));

    let bytes = postcard::to_allocvec(&first).unwrap();
    let round_trip: Facts = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(round_trip, first);
}

/// The point of taking TypeScript before any other language: it has two type
/// containers where Rust has three and Python has one, and #31's vocabulary
/// has to keep them apart rather than flattening both onto `Class`.
#[test]
fn a_typescript_class_and_interface_land_on_distinct_def_kinds() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "Client").kind.to_string(), "class");
    assert_eq!(def(&facts, "ClientOptions").kind.to_string(), "interface");
    assert_eq!(def(&facts, "Transport").kind.to_string(), "class");
    assert_eq!(def(&facts, "TransportName").kind.to_string(), "type-alias");
    assert_eq!(def(&facts, "Ceiling").kind.to_string(), "enum");
    assert_eq!(def(&facts, "Reserved").kind.to_string(), "namespace");
    assert_eq!(def(&facts, "constructor").kind.to_string(), "constructor");
    assert_eq!(def(&facts, "attempts").kind.to_string(), "property");
    assert_eq!(def(&facts, "send").kind.to_string(), "method");
    assert_eq!(def(&facts, "secret").kind.to_string(), "field");
}

/// TypeScript spells visibility four ways, and only one of them is a name.
#[test]
fn a_typescript_member_reports_the_visibility_it_declares() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    let visibility = |name: &str| format!("{:?}", def(&facts, name).visibility);
    assert_eq!(visibility("#attempts"), "Private");
    assert_eq!(visibility("secret"), "Private");
    assert_eq!(visibility("reset"), "Private");
    assert_eq!(
        facts
            .defs()
            .iter()
            .find(|candidate| candidate.local_qualified.as_deref() == Some("Client.shared"))
            .unwrap()
            .visibility,
        Visibility::Protected
    );
    assert_eq!(visibility("baseUrl"), "Public");
    assert_eq!(visibility("send"), "Public");
    // Nesting is spelled the language's way, not Rust's.
    assert_eq!(
        def(&facts, "send").local_qualified.as_deref(),
        Some("Client.send")
    );
}

/// An arrow-function binding is a function; a value binding is not. Neither
/// distinction is in the grammar — both come out of `typescript_refine`.
#[test]
fn a_typescript_binding_is_a_function_only_when_its_initializer_is_one() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "makeClient").kind.to_string(), "function");
    assert_eq!(def(&facts, "DEFAULT_CEILING").kind.to_string(), "variable");
    assert_eq!(def(&facts, "INTERNAL_PREFIX").kind.to_string(), "variable");
    // A binding whose initializer starts on the next line: the signature ends
    // at the line break, so there is nothing left to read it from.
    assert_eq!(def(&facts, "later").kind.to_string(), "variable");
}

/// Documentation is the comment run before the declaration, and a body that
/// opens with a string literal is body, not documentation.
#[test]
fn typescript_documentation_is_the_comment_run_and_not_the_body() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    let ceiling = def(&facts, "DEFAULT_CEILING");
    assert!(ceiling.doc_idents.iter().any(|ident| ident == "ceiling"));
    // The boundary the query header states: a binding that is not exported is
    // matched by a parent-anchored pattern, and a parent-anchored pattern
    // cannot carry a comment run. It is indexed; it is not documented.
    assert!(def(&facts, "INTERNAL_PREFIX").doc_idents.is_empty());

    let join = def(&facts, "join");
    assert!(join.doc_idents.iter().any(|ident| ident == "Joins"));
    assert!(join.body_idents.iter().any(|ident| ident == "strict"));

    // A decorator reaches back into the span, exactly as a Python decorator does.
    assert!(def(&facts, "Client")
        .attribute_idents
        .iter()
        .any(|ident| ident == "sealed"));
}

#[test]
fn a_typescript_var_binding_is_indexed() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "legacy").kind.to_string(), "variable");
}

#[test]
fn an_exported_var_binding_is_documented() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    let shared = facts
        .defs()
        .iter()
        .find(|candidate| candidate.name == "shared" && candidate.kind.to_string() == "variable")
        .expect("no exported var named shared");
    assert!(shared.doc_idents.iter().any(|ident| ident == "Shared"));
}

#[test]
fn an_enum_member_is_a_field() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "Ceiling").kind.to_string(), "enum");
    assert_eq!(def(&facts, "Http").kind.to_string(), "field");
}

#[test]
fn an_enum_member_without_a_value_is_still_a_field() {
    let facts = extract(Lang::TypeScript, b"enum Plain { A, B }\n");
    assert_eq!(def(&facts, "Plain").kind.to_string(), "enum");
    assert_eq!(def(&facts, "A").kind.to_string(), "field");
    assert_eq!(def(&facts, "B").kind.to_string(), "field");
}

#[test]
fn a_non_exported_binding_is_private() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(
        def(&facts, "INTERNAL_PREFIX").visibility,
        Visibility::Private
    );
}

#[test]
fn an_exported_binding_stays_public() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(
        def(&facts, "DEFAULT_CEILING").visibility,
        Visibility::Public
    );
}

#[test]
fn a_non_exported_declaration_is_private() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "join").visibility, Visibility::Private);
}

#[test]
fn a_declared_modifier_still_beats_the_capture() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    assert_eq!(def(&facts, "#attempts").visibility, Visibility::Private);
}

#[test]
fn a_typescript_fixture_has_a_readable_golden_projection() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    insta::assert_yaml_snapshot!("typescript_surface", to_snapshot("surface.ts", &facts));
}

/// A `.d.ts` is nothing but ambient declarations, and ambient declarations
/// nest the ordinary declaration one level deeper than the query would
/// otherwise reach. Getting this wrong is quiet rather than loud — the inner
/// node still matches, so the definitions are all found and only `declare`
/// falls off the front of every signature in the file.
#[test]
fn a_typescript_ambient_declaration_keeps_the_declare_keyword() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "ambient.d.ts");
    assert!(matches!(
        facts.status(),
        ParseStatus::Tags {
            parse_errors: false
        }
    ));
    assert_eq!(
        def(&facts, "preload").signature,
        "declare function preload(name: string): void;"
    );
    assert_eq!(
        def(&facts, "start").signature,
        "export declare function start(env: HostEnv): Runtime;"
    );
    assert!(def(&facts, "preload")
        .doc_idents
        .iter()
        .any(|ident| ident == "transport"));

    // Nesting deeper must not cost the members or the modifiers on them.
    assert_eq!(def(&facts, "token").visibility, Visibility::Private);
    assert_eq!(def(&facts, "release").kind.to_string(), "method");

    // Declaration merging is two definitions sharing a name, not one of them
    // overwriting the other: `Session` is a class here *and* a namespace.
    let sessions: Vec<_> = facts
        .defs()
        .iter()
        .filter(|def| def.name == "Session")
        .map(|def| def.kind.to_string())
        .collect();
    assert_eq!(sessions, vec!["class", "namespace"]);

    insta::assert_yaml_snapshot!("typescript_ambient", to_snapshot("ambient.d.ts", &facts));
}

#[test]
fn a_recovered_typescript_fixture_records_parse_errors() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "recovered.ts");
    assert!(matches!(
        facts.status(),
        ParseStatus::Tags { parse_errors: true }
    ));
    assert_eq!(def(&facts, "before").kind.to_string(), "function");
    assert_eq!(def(&facts, "After").kind.to_string(), "class");
    insta::assert_yaml_snapshot!("typescript_recovered", to_snapshot("recovered.ts", &facts));
}

/// `.ts` and `.tsx` are two grammars, not one grammar and a file extension.
/// The same bytes are a clean parse under one and a broken one under the
/// other, which is the only evidence that both are really wired up.
#[test]
fn a_tsx_file_is_parsed_by_the_tsx_grammar_not_the_ts_one() {
    let content = fs::read(fixture("typescript", "surface.tsx")).unwrap();
    let under_tsx = extract(Lang::Tsx, &content);
    let under_typescript = extract(Lang::TypeScript, &content);

    assert!(matches!(
        under_tsx.status(),
        ParseStatus::Tags {
            parse_errors: false
        }
    ));
    assert!(matches!(
        under_typescript.status(),
        ParseStatus::Tags { parse_errors: true }
    ));
    assert_eq!(def(&under_tsx, "Badge").kind.to_string(), "function");
    assert_eq!(def(&under_tsx, "BadgeList").kind.to_string(), "class");
    assert_ne!(under_tsx.defs(), under_typescript.defs());
}

#[test]
fn a_tsx_fixture_has_a_readable_golden_projection() {
    let (_, facts) = facts_for(Lang::Tsx, "typescript", "surface.tsx");
    insta::assert_yaml_snapshot!("tsx_surface", to_snapshot("surface.tsx", &facts));
}

#[test]
fn typescript_signature_text_is_sliced_from_the_span_not_reconstructed() {
    let (content, facts) = facts_for(Lang::TypeScript, "typescript", "surface.ts");
    let source = std::str::from_utf8(&content).unwrap();
    for def in facts.defs() {
        let raw = &source
            [def.signature_span.start_byte() as usize..def.signature_span.end_byte() as usize];
        assert_eq!(def.signature, display_signature(raw));
    }
}

#[test]
fn a_python_imports_fixture_has_a_readable_golden_projection() {
    let (_, facts) = facts_for(Lang::Python, "python", "imports.py");
    insta::assert_yaml_snapshot!("python_imports", to_snapshot("imports.py", &facts));
}

#[test]
fn a_typescript_imports_fixture_has_a_readable_golden_projection() {
    let (_, facts) = facts_for(Lang::TypeScript, "typescript", "imports.ts");
    insta::assert_yaml_snapshot!("typescript_imports", to_snapshot("imports.ts", &facts));
}

/// The audit that should have run in #31: every `ImportKind` variant is either
/// produced by something or a promise the schema does not keep. The union over
/// the Rust, Python and TypeScript import fixtures is compared against
/// `ImportKind::ALL` rather than a list written out here, so a variant added
/// without a producer fails this test instead of shipping as a field no
/// extractor ever fills.
#[test]
fn every_import_kind_variant_has_a_producer() {
    use rr_core::ImportKind;

    let mut produced = std::collections::BTreeSet::new();
    for (lang, language, name) in [
        (Lang::Rust, "rust", "imports.rs"),
        (Lang::Python, "python", "imports.py"),
        (Lang::TypeScript, "typescript", "imports.ts"),
    ] {
        let (_, facts) = facts_for(lang, language, name);
        for import in facts.imports() {
            produced.insert(import.kind);
        }
    }
    // `BTreeSet` iterates in `Ord` order, which is declaration order, which is
    // the order of `ALL`.
    assert_eq!(
        produced.into_iter().collect::<Vec<_>>(),
        ImportKind::ALL.to_vec()
    );
}

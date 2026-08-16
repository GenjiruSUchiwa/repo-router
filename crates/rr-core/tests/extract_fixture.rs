#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rr_core::parser::RustExtractor;
use rr_core::EXTRACTOR_VERSION;
use rr_core::{DefKind, DegradedReason, Facts, ParseStatus, ReferenceKind, FACT_SCHEMA_VERSION};
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
struct SnapshotDef {
    name: String,
    local_qualified: Option<String>,
    kind: String,
    visibility: String,
    start_line: u32,
    end_line: u32,
    signature: String,
    declaration: String,
    body_preview: String,
    test_explicit: bool,
    test_cfg: bool,
    doc_idents: Vec<String>,
    attribute_idents: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotRef {
    name: String,
    qualified: Option<String>,
    kind: String,
    line: u32,
    owner: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
struct SnapshotImport {
    kind: String,
    path: String,
    alias: Option<String>,
    is_public: bool,
    is_glob: bool,
    owner: Option<String>,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn extract_path(path: &Path) -> (String, Facts) {
    let bytes = fs::read(path).unwrap();
    let mut extractor = RustExtractor::new().unwrap();
    let facts = extractor.extract(&bytes).unwrap();
    let source = String::from_utf8_lossy(&bytes).into_owned();
    validate_spans(&facts, &source);
    (source, facts)
}

fn validate_spans(facts: &Facts, source: &str) {
    if matches!(facts.status(), ParseStatus::Degraded { .. }) {
        return;
    }
    for def in facts.defs() {
        def.span.validate_for(source).unwrap();
        def.signature_span.validate_for(source).unwrap();
    }
    for reference in facts.references() {
        reference.span.validate_for(source).unwrap();
    }
    for import in facts.imports() {
        import.span.validate_for(source).unwrap();
    }
}

fn status_label(status: ParseStatus) -> String {
    match status {
        ParseStatus::Complete => "complete".into(),
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

fn visibility_label(vis: &rr_core::Visibility) -> String {
    match vis {
        rr_core::Visibility::Private => "private".into(),
        rr_core::Visibility::Public => "public".into(),
        rr_core::Visibility::Protected => "protected".into(),
        rr_core::Visibility::Internal => "internal".into(),
        rr_core::Visibility::Crate => "crate".into(),
        rr_core::Visibility::Restricted(path) => format!("restricted({path})"),
    }
}

fn owner_name(facts: &Facts, id: Option<rr_core::LocalDefId>) -> Option<String> {
    id.and_then(|id| facts.def(id).map(|d| d.name.clone()))
}

fn slice_preview(source: &str, start: u32, end: u32) -> String {
    let text = &source[start as usize..end as usize];
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        let trimmed: String = flat.chars().take(80).collect();
        format!("{trimmed}…")
    } else {
        flat
    }
}

fn to_snapshot(path: &str, source: &str, facts: &Facts) -> SnapshotFile {
    SnapshotFile {
        path: path.to_string(),
        status: status_label(facts.status()),
        defs: facts
            .defs()
            .iter()
            .map(|def| SnapshotDef {
                name: def.name.clone(),
                local_qualified: def.local_qualified.clone(),
                kind: def.kind.as_str().to_string(),
                visibility: visibility_label(&def.visibility),
                start_line: def.span.start_line(),
                end_line: def.span.end_line(),
                signature: slice_preview(
                    source,
                    def.signature_span.start_byte(),
                    def.signature_span.end_byte(),
                ),
                declaration: def.signature.clone(),
                body_preview: def
                    .body_idents
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
                test_explicit: def.test_signals.explicit_attribute,
                test_cfg: def.test_signals.inside_cfg_test,
                doc_idents: def.doc_idents.clone(),
                attribute_idents: def.attribute_idents.clone(),
            })
            .collect(),
        references: facts
            .references()
            .iter()
            .map(|reference| SnapshotRef {
                name: reference.name.clone(),
                qualified: reference.qualified.clone(),
                kind: format!("{:?}", reference.kind),
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
                alias: import.alias.clone(),
                is_public: import.is_public,
                is_glob: import.is_glob,
                owner: owner_name(facts, import.owner),
            })
            .collect(),
    }
}

fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_rust_files(root, &mut files);
    files.sort();
    files
}

fn walk_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn rust_basic_golden_and_verify_token_refs() {
    let root = workspace_root().join("fixtures/rust-basic");
    assert!(root.is_dir(), "missing fixture root {root:?}");

    let mut extractor = RustExtractor::new().unwrap();
    let mut all_defs = Vec::new();
    let mut snapshots = BTreeMap::new();

    for path in collect_rust_files(&root) {
        let rel = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path).unwrap();
        let facts = extractor.extract(&bytes).unwrap();
        let source = std::str::from_utf8(&bytes).unwrap();
        validate_spans(&facts, source);
        for def in facts.defs() {
            all_defs.push(def.name.clone());
        }
        snapshots.insert(rel.clone(), to_snapshot(&rel, source, &facts));

        if rel.ends_with("auth/token.rs") {
            let verify = facts
                .defs()
                .iter()
                .find(|d| d.name == "verify_token")
                .expect("verify_token definition");
            let owned: Vec<_> = facts
                .references()
                .iter()
                .filter(|r| {
                    r.owner
                        .and_then(|id| facts.def(id))
                        .is_some_and(|d| d.name == "verify_token")
                })
                .collect();
            let summary: Vec<_> = owned
                .iter()
                .map(|r| (r.name.as_str(), r.kind, r.span.start_line()))
                .collect();
            assert_eq!(
                summary,
                vec![
                    ("decode_jwt", ReferenceKind::Call, 24),
                    ("now", ReferenceKind::Call, 25),
                    ("find_user", ReferenceKind::Call, 25),
                    ("is_some", ReferenceKind::MethodCall, 25),
                ],
                "verify_token references mismatch: {summary:?}"
            );
            assert_eq!(verify.kind, DefKind::Function);
        }
    }

    all_defs.sort();
    assert_eq!(
        all_defs,
        vec![
            "Claims",
            "User",
            "auth",
            "db",
            "decode_jwt",
            "find_user",
            "main",
            "now",
            "refresh_token",
            "test_token_dummy",
            "token",
            "users",
            "verify_token",
        ]
    );

    for path in collect_rust_files(&root) {
        let bytes = fs::read(&path).unwrap();
        let first = extractor.extract(&bytes).unwrap();
        let second = extractor.extract(&bytes).unwrap();
        assert_eq!(first, second);
        let bytes_round = postcard::to_allocvec(&first).unwrap();
        let back: Facts = postcard::from_bytes(&bytes_round).unwrap();
        assert_eq!(back, first);
    }

    insta::assert_yaml_snapshot!("rust_basic", snapshots);
}

#[test]
fn rust_surface_golden() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/surface.rs");
    let (source, facts) = extract_path(&path);
    assert!(matches!(facts.status(), ParseStatus::Complete));

    let kinds: std::collections::BTreeSet<_> = facts.defs().iter().map(|d| d.kind).collect();
    for kind in [
        DefKind::Function,
        DefKind::Method,
        DefKind::TraitMethod,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::Union,
        DefKind::Trait,
        DefKind::TypeAlias,
        DefKind::AssociatedType,
        DefKind::Const,
        DefKind::Static,
        DefKind::Module,
        DefKind::Macro,
    ] {
        assert!(kinds.contains(&kind), "missing {kind:?}");
    }

    assert!(facts.defs().iter().any(|d| d.name == "méthode"));
    assert!(facts
        .defs()
        .iter()
        .any(|d| d.name == "unit_test_surface" && d.test_signals.explicit_attribute));
    assert!(facts
        .defs()
        .iter()
        .any(|d| d.name == "nested" && d.kind == DefKind::Module));

    let a = {
        let mut extractor = RustExtractor::new().unwrap();
        extractor.extract(source.as_bytes()).unwrap()
    };
    assert_eq!(a, facts);

    insta::assert_yaml_snapshot!("rust_surface", to_snapshot("surface.rs", &source, &facts));
}

#[test]
fn rust_imports_golden() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/imports.rs");
    let (source, facts) = extract_path(&path);
    assert!(matches!(facts.status(), ParseStatus::Complete));

    let paths: Vec<_> = facts
        .imports()
        .iter()
        .map(|i| (i.path.as_str(), i.alias.as_deref(), i.is_glob, i.is_public))
        .collect();
    assert!(paths
        .iter()
        .any(|(p, a, g, _)| *p == "crate::a::b" && a.is_none() && !*g));
    assert!(paths
        .iter()
        .any(|(p, a, g, _)| *p == "crate::a" && a.is_none() && !*g));
    assert!(paths
        .iter()
        .any(|(p, a, ..)| *p == "crate::a::c" && *a == Some("d")));
    assert!(paths
        .iter()
        .any(|(p, _, g, _)| *p == "crate::a::nested::*" && *g));
    assert!(paths.iter().any(|(p, a, _, pub_)| {
        *p == "crate::internal::Client" && *a == Some("ApiClient") && *pub_
    }));
    assert!(paths
        .iter()
        .any(|(p, a, ..)| *p == "alloc" && *a == Some("allocator")));
    assert!(paths.iter().any(|(p, a, ..)| *p == "a" && *a == Some("_")));
    assert!(facts
        .imports()
        .iter()
        .any(|i| i.path == "crate::z" && i.owner.is_some()));

    insta::assert_yaml_snapshot!("rust_imports", to_snapshot("imports.rs", &source, &facts));
}

#[test]
fn recovered_fixture_retains_neighbors() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/recovered.rs");
    let (_source, facts) = extract_path(&path);
    assert!(matches!(facts.status(), ParseStatus::Recovered { .. }));
    let names: Vec<_> = facts.defs().iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"before_error"));
    assert!(names.contains(&"after_error"));
}

#[test]
fn invalid_utf8_fixture_degrades() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/invalid-utf8.bin");
    let bytes = fs::read(path).unwrap();
    let mut extractor = RustExtractor::new().unwrap();
    let facts = extractor.extract(&bytes).unwrap();
    assert!(matches!(
        facts.status(),
        ParseStatus::Degraded {
            reason: DegradedReason::InvalidUtf8,
            ..
        }
    ));
    assert!(facts.defs().is_empty());
    assert!(facts.references().is_empty());
    assert!(facts.imports().is_empty());
    assert!(facts.lexical_idents().iter().any(|i| i == "alpha"));
    assert!(facts.lexical_idents().iter().any(|i| i == "gamma"));
}

/// The vocabulary widened; Rust's use of it did not.
///
/// Half the claim is here and half is in the `insta` goldens above. The goldens
/// pin the exact kind, visibility, and import kind of every definition in the
/// fixtures, and they were not regenerated for #31 — a changed value would fail
/// them. This test states the other half, which a golden cannot: that none of
/// the variants #31 added is reachable from Rust source at all, so no future
/// edit to `rust.rs` can quietly start emitting one.
#[test]
fn a_rust_repository_indexes_identically_across_the_schema_bump() {
    // Everything `DefKind` held before #31 widened it.
    const RUST_KINDS: [DefKind; 13] = [
        DefKind::Function,
        DefKind::Method,
        DefKind::TraitMethod,
        DefKind::Struct,
        DefKind::Enum,
        DefKind::Union,
        DefKind::Trait,
        DefKind::TypeAlias,
        DefKind::AssociatedType,
        DefKind::Const,
        DefKind::Static,
        DefKind::Module,
        DefKind::Macro,
    ];

    let root = workspace_root().join("fixtures/rust-basic");
    let mut paths = collect_rust_files(&root);
    paths.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/surface.rs"));
    paths.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust/imports.rs"));

    let mut extractor = RustExtractor::new().unwrap();
    let mut seen_defs = 0_usize;
    for path in &paths {
        let bytes = fs::read(path).unwrap();
        let facts = extractor.extract(&bytes).unwrap();

        for def in facts.defs() {
            seen_defs += 1;
            assert!(
                RUST_KINDS.contains(&def.kind),
                "{path:?} produced {:?}, which Rust had no way to spell before #31",
                def.kind
            );
            assert!(
                matches!(
                    def.visibility,
                    rr_core::Visibility::Private
                        | rr_core::Visibility::Public
                        | rr_core::Visibility::Crate
                        | rr_core::Visibility::Restricted(_)
                ),
                "{path:?} produced {:?}, a visibility Rust does not have",
                def.visibility
            );
            // `inside_cfg_test` keeps meaning `#[cfg(test)]`; the neutral field
            // beside it belongs to languages that state test scope some other
            // way, and Rust must never set it.
            assert!(
                !def.test_signals.inside_test_scope,
                "{path:?} set the language-neutral test signal on `{}`",
                def.name
            );
        }

        for import in facts.imports() {
            assert!(
                matches!(
                    import.kind,
                    rr_core::ImportKind::Use | rr_core::ImportKind::ExternCrate
                ),
                "{path:?} produced {:?}, an import kind Rust does not have",
                import.kind
            );
        }
    }
    assert!(seen_defs > 0, "the fixtures produced no definitions");
}

#[test]
fn versions_are_pinned() {
    assert_eq!(EXTRACTOR_VERSION, 3);
    assert_eq!(FACT_SCHEMA_VERSION, 4);
}

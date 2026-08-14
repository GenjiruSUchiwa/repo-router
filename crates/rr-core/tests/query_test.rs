#![allow(clippy::unwrap_used)]

use rr_core::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::RustExtractor;
use rr_core::path::RelPath;
use rr_core::query::{finish_exact, parse_query, route_exact, ExactAtomKind, QueryRequest};
use rr_core::result::{Confidence, NoneReason, Pipeline, QueryResult, TargetId};
use rr_core::ParseStatus;

fn build_test_snapshot(files: Vec<(&str, &[u8], bool)>) -> rr_core::index::Snapshot {
    let mut extractor = RustExtractor::new().unwrap();
    let mut inputs = Vec::new();

    for (index, (path_str, code, generated)) in files.into_iter().enumerate() {
        let facts = extractor.extract(code).unwrap();
        let mut raw_oid = [0u8; 32];
        raw_oid[0] = u8::try_from(index + 1).unwrap();
        inputs.push(FileInput {
            path: RelPath::new(path_str).unwrap(),
            oid: Oid::from_raw(&raw_oid).unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated,
            language: Lang::Rust,
            parse_status: ParseStatus::Complete,
            facts,
        });
    }

    let meta = SnapshotMeta::new(None, true);
    let (snapshot, _) = SnapshotBuilder::new(meta).build(inputs).unwrap();
    snapshot
}

#[test]
fn test_parse_query_empty_error() {
    let snapshot = build_test_snapshot(vec![("src/lib.rs", b"pub fn foo() {}", false)]);
    assert!(parse_query(&snapshot, QueryRequest::new("", None)).is_err());
    assert!(parse_query(&snapshot, QueryRequest::new("   \n\t  ", None)).is_err());
}

#[test]
fn test_atom_classification_rules() {
    let snapshot = build_test_snapshot(vec![
        (
            "src/auth/token.rs",
            b"pub fn verify_token() {}\npub struct Session;",
            false,
        ),
        ("Cargo.toml", b"", false),
    ]);

    let q1 = parse_query(&snapshot, QueryRequest::new("verify_token", None)).unwrap();
    assert_eq!(q1.exact_atoms.len(), 1);
    assert_eq!(q1.exact_atoms[0].kind, ExactAtomKind::Symbol);
    assert_eq!(q1.exact_atoms[0].text, "verify_token");

    let q2 = parse_query(
        &snapshot,
        QueryRequest::new("AuthService::verify_token", None),
    )
    .unwrap();
    assert_eq!(q2.exact_atoms.len(), 1);
    assert_eq!(q2.exact_atoms[0].kind, ExactAtomKind::Qualified);
    assert_eq!(q2.exact_atoms[0].text, "AuthService::verify_token");

    let q3 = parse_query(&snapshot, QueryRequest::new("src/auth/token.rs", None)).unwrap();
    assert_eq!(q3.exact_atoms.len(), 1);
    assert_eq!(q3.exact_atoms[0].kind, ExactAtomKind::Path);
    assert_eq!(q3.exact_atoms[0].text, "src/auth/token.rs");

    let q4 = parse_query(&snapshot, QueryRequest::new("Cargo.toml", None)).unwrap();
    assert_eq!(q4.exact_atoms.len(), 1);
    assert_eq!(q4.exact_atoms[0].kind, ExactAtomKind::Path);
    assert_eq!(q4.exact_atoms[0].text, "Cargo.toml");

    let q5 = parse_query(&snapshot, QueryRequest::new("session", None)).unwrap();
    assert_eq!(q5.exact_atoms.len(), 1);
    assert_eq!(q5.exact_atoms[0].kind, ExactAtomKind::Symbol);

    let q6 = parse_query(
        &snapshot,
        QueryRequest::new("where is session handled", None),
    )
    .unwrap();
    assert!(q6.exact_atoms.is_empty());

    let q7 = parse_query(
        &snapshot,
        QueryRequest::new("where is verify_token handled", None),
    )
    .unwrap();
    assert_eq!(q7.exact_atoms.len(), 1);
    assert_eq!(q7.exact_atoms[0].kind, ExactAtomKind::Symbol);
    assert_eq!(q7.exact_atoms[0].text, "verify_token");

    let q8 = parse_query(
        &snapshot,
        QueryRequest::new("`AuthService::verify_token`?", None),
    )
    .unwrap();
    assert_eq!(q8.exact_atoms.len(), 1);
    assert_eq!(q8.exact_atoms[0].kind, ExactAtomKind::Qualified);
    assert_eq!(q8.exact_atoms[0].text, "AuthService::verify_token");
}

#[test]
fn test_exact_routing_unique_symbol_direct() {
    let snapshot = build_test_snapshot(vec![(
        "src/auth/token.rs",
        b"pub fn verify_token() -> bool { true }",
        false,
    )]);

    let parsed = parse_query(&snapshot, QueryRequest::new("verify_token", None)).unwrap();
    let outcome = route_exact(&snapshot, &parsed);
    let result = finish_exact(outcome);

    match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidate.confidence, Some(Confidence::ONE));
            assert!(matches!(candidate.target, TargetId::Symbol(_)));
        }
        _ => panic!("expected direct result"),
    }
}

#[test]
fn test_exact_routing_unique_file_direct() {
    let snapshot = build_test_snapshot(vec![(
        "src/auth/token.rs",
        b"pub fn verify_token() -> bool { true }",
        false,
    )]);

    let parsed = parse_query(&snapshot, QueryRequest::new("src/auth/token.rs", None)).unwrap();
    let outcome = route_exact(&snapshot, &parsed);
    let result = finish_exact(outcome);

    match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidate.confidence, Some(Confidence::ONE));
            assert!(matches!(candidate.target, TargetId::File(_)));
        }
        _ => panic!("expected file direct result"),
    }
}

#[test]
fn test_exact_routing_path_filter() {
    let snapshot = build_test_snapshot(vec![
        ("src/auth/token.rs", b"pub fn parse() {}", false),
        ("src/parser/token.rs", b"pub fn parse() {}", false),
    ]);

    let rel = RelPath::new("src/auth/token.rs").unwrap();
    let parsed = parse_query(&snapshot, QueryRequest::new("parse", Some(&rel))).unwrap();
    let outcome = route_exact(&snapshot, &parsed);
    let result = finish_exact(outcome);

    match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidate.confidence, Some(Confidence::ONE));
            let TargetId::Symbol(sym_id) = candidate.target else {
                panic!()
            };
            let sym = &snapshot.symbols[sym_id.index()];
            let file = &snapshot.files[sym.file.index()];
            assert_eq!(snapshot.strings[file.path.index()], "src/auth/token.rs");
        }
        _ => panic!("expected direct result with path filter"),
    }

    let non_existent_rel = RelPath::new("src/other/path.rs").unwrap();
    let parsed_miss = parse_query(
        &snapshot,
        QueryRequest::new("parse", Some(&non_existent_rel)),
    )
    .unwrap();
    let outcome_miss = route_exact(&snapshot, &parsed_miss);
    assert_eq!(
        finish_exact(outcome_miss),
        QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Exact
        }
    );
}

#[test]
fn test_disambiguation_positive_context_overlap() {
    let snapshot = build_test_snapshot(vec![
        ("src/auth/session.rs", b"pub fn session_handler() {}", false),
        ("src/net/session.rs", b"pub fn session_handler() {}", false),
    ]);

    let parsed_ambiguous =
        parse_query(&snapshot, QueryRequest::new("session_handler", None)).unwrap();
    let outcome_ambiguous = route_exact(&snapshot, &parsed_ambiguous);
    let result_ambiguous = finish_exact(outcome_ambiguous);

    match result_ambiguous {
        QueryResult::Candidates {
            candidates,
            pipeline,
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidates.len(), 2);
            assert_eq!(candidates[0].confidence, None);
            assert_eq!(candidates[1].confidence, None);
        }
        _ => panic!("expected 2 candidates"),
    }

    let parsed_auth =
        parse_query(&snapshot, QueryRequest::new("auth session_handler", None)).unwrap();
    let outcome_auth = route_exact(&snapshot, &parsed_auth);
    let result_auth = finish_exact(outcome_auth);

    match result_auth {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidate.confidence, Some(Confidence::ONE));
            let TargetId::Symbol(sym_id) = candidate.target else {
                panic!()
            };
            let sym = &snapshot.symbols[sym_id.index()];
            let file = &snapshot.files[sym.file.index()];
            assert_eq!(snapshot.strings[file.path.index()], "src/auth/session.rs");
        }
        _ => panic!("expected direct disambiguated result"),
    }
}
#[test]
fn test_disambiguation_query_path_qualifier() {
    let snapshot = build_test_snapshot(vec![
        ("src/auth/token.rs", b"pub fn parse_token() {}", false),
        ("src/parser/token.rs", b"pub fn parse_token() {}", false),
    ]);

    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("src/auth/token.rs parse_token", None),
    )
    .unwrap();
    let outcome = route_exact(&snapshot, &parsed);
    let result = finish_exact(outcome);

    match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidate.confidence, Some(Confidence::ONE));
            let TargetId::Symbol(sym_id) = candidate.target else {
                panic!()
            };
            let sym = &snapshot.symbols[sym_id.index()];
            let file = &snapshot.files[sym.file.index()];
            assert_eq!(snapshot.strings[file.path.index()], "src/auth/token.rs");
        }
        _ => panic!("expected direct result from path qualifier in query"),
    }
}

#[test]
fn test_bounded_top_three_candidates() {
    let snapshot = build_test_snapshot(vec![
        ("src/a/mod.rs", b"pub struct TargetItem;", false),
        ("src/b/mod.rs", b"pub struct TargetItem;", false),
        ("src/c/mod.rs", b"pub struct TargetItem;", false),
        ("src/d/mod.rs", b"pub struct TargetItem;", false),
        ("src/e/mod.rs", b"pub struct TargetItem;", false),
    ]);

    let parsed = parse_query(&snapshot, QueryRequest::new("TargetItem", None)).unwrap();
    let outcome = route_exact(&snapshot, &parsed);
    let result = finish_exact(outcome);

    match result {
        QueryResult::Candidates {
            candidates,
            pipeline,
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert_eq!(candidates.len(), 3);
            for c in &candidates {
                assert_eq!(c.confidence, None);
            }
        }
        _ => panic!("expected 3 bounded candidates"),
    }
}

#[test]
fn test_permutation_determinism() {
    let file_specs = [
        (
            "src/core/auth.rs",
            "pub fn verify() {}\npub struct Session;\n",
        ),
        (
            "src/net/auth.rs",
            "pub fn verify() {}\npub struct Session;\n",
        ),
        (
            "src/db/auth.rs",
            "pub fn verify() {}\npub struct Session;\n",
        ),
        (
            "src/api/auth.rs",
            "pub fn verify() {}\npub struct Session;\n",
        ),
    ];

    let base_snapshot = {
        let list = file_specs
            .iter()
            .map(|(p, c)| (*p, c.as_bytes(), false))
            .collect();
        build_test_snapshot(list)
    };

    let base_parsed = parse_query(&base_snapshot, QueryRequest::new("verify", None)).unwrap();
    let base_outcome = route_exact(&base_snapshot, &base_parsed);
    let base_result = finish_exact(base_outcome);

    for i in 0_u32..100 {
        let mut permuted = file_specs;
        let mut seed = i + 1;
        for index in (1..permuted.len()).rev() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let swap_index = seed as usize % (index + 1);
            permuted.swap(index, swap_index);
        }
        let list = permuted
            .iter()
            .map(|(p, c)| (*p, c.as_bytes(), false))
            .collect();
        let snapshot = build_test_snapshot(list);

        let parsed = parse_query(&snapshot, QueryRequest::new("verify", None)).unwrap();
        let outcome = route_exact(&snapshot, &parsed);
        let result = finish_exact(outcome);

        assert_eq!(result.exit_code(), base_result.exit_code());
        match (&base_result, &result) {
            (
                QueryResult::Candidates { candidates: c1, .. },
                QueryResult::Candidates { candidates: c2, .. },
            ) => {
                assert_eq!(c1.len(), c2.len());
                for (a, b) in c1.iter().zip(c2.iter()) {
                    let TargetId::Symbol(sa) = a.target else {
                        panic!()
                    };
                    let TargetId::Symbol(sb) = b.target else {
                        panic!()
                    };
                    let sym_a = &base_snapshot.symbols[sa.index()];
                    let sym_b = &snapshot.symbols[sb.index()];
                    assert_eq!(
                        base_snapshot.strings[sym_a.qualified_name.index()],
                        snapshot.strings[sym_b.qualified_name.index()]
                    );
                }
            }
            _ => panic!("expected candidates in permutation test"),
        }
    }
}

#[test]
fn test_unicode_exact_symbol_and_normalized_path() {
    let snapshot = build_test_snapshot(vec![("src/é.rs", "pub fn é() {}".as_bytes(), false)]);

    let parsed_symbol = parse_query(&snapshot, QueryRequest::new("é", None)).unwrap();
    assert!(matches!(
        finish_exact(route_exact(&snapshot, &parsed_symbol)),
        QueryResult::Direct { .. }
    ));

    let parsed_path = parse_query(&snapshot, QueryRequest::new("./src/é.rs", None)).unwrap();
    let QueryResult::Direct { candidate, .. } = finish_exact(route_exact(&snapshot, &parsed_path))
    else {
        panic!("expected normalized path direct result");
    };
    assert!(matches!(candidate.target, TargetId::File(_)));
}

#[test]
fn test_exact_atom_grammar_is_conservative() {
    let snapshot = build_test_snapshot(vec![(
        "src/lib.rs",
        b"pub fn verify_token() {}\npub fn foo() {}",
        false,
    )]);

    let punctuation = parse_query(&snapshot, QueryRequest::new("foo:", None)).unwrap();
    assert!(punctuation.exact_atoms.is_empty());

    let unicode_space = parse_query(
        &snapshot,
        QueryRequest::new("verify_token\u{00a0}foo", None),
    )
    .unwrap();
    assert!(unicode_space.exact_atoms.is_empty());
}

#[test]
fn test_first_indexed_path_atom_qualifies_symbol() {
    let snapshot = build_test_snapshot(vec![
        ("src/a.rs", b"pub fn verify_token() {}", false),
        ("src/b.rs", b"pub fn verify_token() {}", false),
    ]);
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("missing/path.rs ./src/a.rs verify_token", None),
    )
    .unwrap();
    let QueryResult::Direct { candidate, .. } = finish_exact(route_exact(&snapshot, &parsed))
    else {
        panic!("expected path-qualified symbol");
    };
    let TargetId::Symbol(symbol_id) = candidate.target else {
        panic!("expected symbol target");
    };
    let symbol = &snapshot.symbols[symbol_id.index()];
    let file = &snapshot.files[symbol.file.index()];
    assert_eq!(snapshot.strings[file.path.index()], "src/a.rs");
}

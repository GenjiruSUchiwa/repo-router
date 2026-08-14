#![allow(clippy::unwrap_used)]

use smallvec::smallvec;

use rr_core::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::RustExtractor;
use rr_core::path::RelPath;
use rr_core::render::{decode_anchor, encode_anchor, render_json, render_text};
use rr_core::result::{Candidate, Confidence, NoneReason, Pipeline, QueryResult, TargetId};
use rr_core::ParseStatus;

fn build_test_snapshot() -> rr_core::index::Snapshot {
    let mut extractor = RustExtractor::new().unwrap();
    let mut inputs = Vec::new();

    let auth_facts = extractor.extract(b"pub fn verify_token() {}\n").unwrap();
    inputs.push(FileInput {
        path: RelPath::new("src/auth/token.rs").unwrap(),
        oid: Oid::from_raw(&[1; 32]).unwrap(),
        representation: ContentRepresentation::RawNoGit,
        generated: false,
        language: Lang::Rust,
        parse_status: ParseStatus::Complete,
        facts: auth_facts,
    });

    let meta = SnapshotMeta::new(None, true);
    let (snapshot, _) = SnapshotBuilder::new(meta).build(inputs).unwrap();
    snapshot
}

#[test]
fn test_encode_anchor_percent_escaping() {
    let path1 = RelPath::new("src/auth/token.rs").unwrap();
    assert_eq!(
        encode_anchor(&path1, Some("verify_token")),
        "src/auth/token.rs#verify_token"
    );
    assert_eq!(encode_anchor(&path1, None), "src/auth/token.rs");

    let path_hash = RelPath::new("src/a#b.rs").unwrap();
    assert_eq!(encode_anchor(&path_hash, None), "src/a%23b.rs");

    let path_percent = RelPath::new("src/percent%.rs").unwrap();
    assert_eq!(encode_anchor(&path_percent, None), "src/percent%25.rs");

    let path_both = RelPath::new("src/a%23b#c%.rs").unwrap();
    assert_eq!(
        encode_anchor(&path_both, Some("foo#bar%baz")),
        "src/a%2523b%23c%25.rs#foo%23bar%25baz"
    );
}

#[test]
fn test_decode_anchor_roundtrip_and_errors() {
    let path = RelPath::new("src/auth/token.rs").unwrap();
    let encoded = encode_anchor(&path, Some("verify_token"));
    let (decoded_path, decoded_sym) = decode_anchor(&encoded).unwrap();
    assert_eq!(decoded_path, path);
    assert_eq!(decoded_sym.as_deref(), Some("verify_token"));

    let path_special = RelPath::new("src/a#b%c.rs").unwrap();
    let encoded_special = encode_anchor(&path_special, Some("fn#name%test"));
    let (dec_p, dec_s) = decode_anchor(&encoded_special).unwrap();
    assert_eq!(dec_p, path_special);
    assert_eq!(dec_s.as_deref(), Some("fn#name%test"));

    assert!(decode_anchor("src/%2").is_err());
    assert!(decode_anchor("src/%ZZ").is_err());
    assert!(decode_anchor("src/%41").is_err());
    assert!(decode_anchor("src/foo#bar#baz").is_err());
}

#[test]
fn test_render_text_contracts() {
    let snapshot = build_test_snapshot();

    let direct_sym = QueryResult::Direct {
        candidate: Candidate::new(
            TargetId::Symbol(snapshot.symbols[0].id),
            Some(Confidence::ONE),
        ),
        pipeline: Pipeline::Exact,
    };
    let text_direct_sym = render_text(&snapshot, &direct_sym).unwrap();
    assert_eq!(
        text_direct_sym,
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token\n"
    );

    let direct_file = QueryResult::Direct {
        candidate: Candidate::new(TargetId::File(snapshot.files[0].id), Some(Confidence::ONE)),
        pipeline: Pipeline::Exact,
    };
    let text_direct_file = render_text(&snapshot, &direct_file).unwrap();
    assert_eq!(
        text_direct_file,
        "FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs\n"
    );

    let candidates = QueryResult::Candidates {
        candidates: smallvec![
            Candidate::new(TargetId::Symbol(snapshot.symbols[0].id), None),
            Candidate::new(TargetId::File(snapshot.files[0].id), None),
        ],
        pipeline: Pipeline::Exact,
    };
    let text_candidates = render_text(&snapshot, &candidates).unwrap();
    assert_eq!(
        text_candidates,
        "source candidates:\n1. src/auth/token.rs#verify_token\n2. src/auth/token.rs\n"
    );

    let none_not_found = QueryResult::None {
        reason: NoneReason::NotFound,
        pipeline: Pipeline::Exact,
    };
    assert_eq!(
        render_text(&snapshot, &none_not_found).unwrap(),
        "NO ANCHOR (index has no match); try: rr map\n"
    );

    let none_low_conf = QueryResult::None {
        reason: NoneReason::LowConfidence,
        pipeline: Pipeline::Exact,
    };
    assert_eq!(
        render_text(&snapshot, &none_low_conf).unwrap(),
        "NO ANCHOR (confidence too low); refine the query or use --path\n"
    );
}

#[test]
fn test_render_json_contracts() {
    let snapshot = build_test_snapshot();

    let direct_sym = QueryResult::Direct {
        candidate: Candidate::new(
            TargetId::Symbol(snapshot.symbols[0].id),
            Some(Confidence::ONE),
        ),
        pipeline: Pipeline::Exact,
    };
    let json_direct_sym = render_json(&snapshot, &direct_sym).unwrap();
    assert!(json_direct_sym.ends_with('\n'));
    assert_eq!(json_direct_sym.lines().count(), 1);
    let val: serde_json::Value = serde_json::from_str(&json_direct_sym).unwrap();
    assert_eq!(val["v"], 1);
    assert_eq!(val["result"], "direct");
    assert_eq!(val["pipeline"], "exact");
    assert_eq!(val["confidence"], 1.0);
    assert_eq!(val["anchor"]["path"], "src/auth/token.rs");
    assert_eq!(val["anchor"]["symbol"], "verify_token");
    assert_eq!(val["anchor"]["lines"], serde_json::json!([1, 1]));

    let direct_file = QueryResult::Direct {
        candidate: Candidate::new(TargetId::File(snapshot.files[0].id), Some(Confidence::ONE)),
        pipeline: Pipeline::Exact,
    };
    let json_direct_file = render_json(&snapshot, &direct_file).unwrap();
    let val_file: serde_json::Value = serde_json::from_str(&json_direct_file).unwrap();
    assert_eq!(val_file["anchor"]["path"], "src/auth/token.rs");
    assert!(val_file["anchor"]["symbol"].is_null());
    assert!(val_file["anchor"]["lines"].is_null());

    let none_res = QueryResult::None {
        reason: NoneReason::NotFound,
        pipeline: Pipeline::Exact,
    };
    let json_none = render_json(&snapshot, &none_res).unwrap();
    let val_none: serde_json::Value = serde_json::from_str(&json_none).unwrap();
    assert_eq!(val_none["v"], 1);
    assert_eq!(val_none["result"], "none");
    assert_eq!(val_none["pipeline"], "exact");
    assert_eq!(val_none["reason"], "not_found");
    assert!(val_none.get("anchor").is_none());
}

#[test]
fn test_unicode_anchor_roundtrip() {
    let path = RelPath::new("src/é.rs").unwrap();
    let encoded = encode_anchor(&path, Some("é"));
    assert_eq!(encoded, "src/é.rs#é");
    let (decoded_path, decoded_symbol) = decode_anchor(&encoded).unwrap();
    assert_eq!(decoded_path, path);
    assert_eq!(decoded_symbol.as_deref(), Some("é"));
}

#[test]
fn test_invalid_result_invariants_fail_before_rendering() {
    let snapshot = build_test_snapshot();
    let missing_confidence = QueryResult::Direct {
        candidate: Candidate::new(TargetId::File(snapshot.files[0].id), None),
        pipeline: Pipeline::Exact,
    };
    assert!(render_text(&snapshot, &missing_confidence).is_err());
    assert!(render_json(&snapshot, &missing_confidence).is_err());

    let empty_candidates = QueryResult::Candidates {
        candidates: smallvec![],
        pipeline: Pipeline::Exact,
    };
    assert!(render_text(&snapshot, &empty_candidates).is_err());
    assert!(render_json(&snapshot, &empty_candidates).is_err());
}

#![allow(clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use proptest::prelude::*;
use rr_core::facts::{Def, Import, Reference};
use rr_core::lex::split::{for_each_lexeme, is_canonical_term};
use rr_core::lex::{
    append_source_terms, lexical_profile, query_terms, FieldTerm, InputKind, LexicalField,
    LexicalProfile, Lexicon, TermId, LEXICAL_VERSION,
};
use rr_core::parser::RustExtractor;
use smallvec::SmallVec;

struct CountingAllocator;

thread_local! {
    static TRACK_ALLOCS_TL: Cell<bool> = const { Cell::new(false) };
    static ALLOC_COUNT_TL: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCS_TL.with(|track| {
            if track.get() {
                ALLOC_COUNT_TL.with(|count| {
                    count.set(count.get() + 1);
                });
            }
        });
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn reset_alloc_counter() {
    ALLOC_COUNT_TL.with(|count| count.set(0));
}

fn set_alloc_tracking(enabled: bool) {
    TRACK_ALLOCS_TL.with(|track| track.set(enabled));
}

fn alloc_count() -> usize {
    ALLOC_COUNT_TL.with(Cell::get)
}

#[test]
fn test_lexical_version_and_profile() {
    let profile = lexical_profile();
    assert_eq!(profile.algorithm, LEXICAL_VERSION);
    assert_eq!(profile.rust_unicode, std::char::UNICODE_VERSION);
    assert_eq!(
        profile.normalization_crate,
        rr_core::lex::NORMALIZATION_CRATE_VERSION
    );

    let serialized = serde_json::to_string(&profile).unwrap();
    let deserialized: LexicalProfile = serde_json::from_str(&serialized).unwrap();
    assert_eq!(profile, deserialized);
}

#[test]
fn test_lexical_field_as_str_and_serde_consistency() {
    for field in LexicalField::ALL {
        let as_str = field.as_str();
        let display = format!("{field}");
        assert_eq!(as_str, display);

        let json = serde_json::to_string(&field).unwrap();
        let expected_json = format!("\"{as_str}\"");
        assert_eq!(json, expected_json);

        let roundtrip: LexicalField = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, field);
    }
}

#[test]
fn test_term_id_indexing_and_serde() {
    let id: TermId = serde_json::from_str("42").unwrap();
    assert_eq!(id.index(), 42);

    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, "42");
    let back: TermId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);

    let postcard_bytes = postcard::to_allocvec(&id).unwrap();
    let postcard_back: TermId = postcard::from_bytes(&postcard_bytes).unwrap();
    assert_eq!(postcard_back, id);
}

#[test]
fn test_query_terms_properties() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "token verification verify",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let q = query_terms("where is token verification handled?", &lexicon);
    assert_eq!(q.len(), 3);
    assert!(!q.is_empty());

    let token_id = lexicon.get("token").unwrap();
    let verification_id = lexicon.get("verification").unwrap();
    let verify_id = lexicon.get("verify").unwrap();

    assert_eq!(q.as_slice(), &[token_id, verification_id, verify_id]);

    let collected: Vec<TermId> = q.iter().collect();
    assert_eq!(collected, vec![token_id, verification_id, verify_id]);

    let into_collected: Vec<TermId> = q.into_iter().collect();
    assert_eq!(into_collected, vec![token_id, verification_id, verify_id]);
}

#[test]
fn test_query_stem_lookup_when_long_term_absent() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "verify",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    assert!(lexicon.get("verification").is_none());
    let verify_id = lexicon.get("verify").unwrap();

    let q = query_terms("verification", &lexicon);
    assert_eq!(q.as_slice(), &[verify_id]);
}

#[test]
fn test_query_stem_lookup_when_short_term_absent() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "verification",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    assert!(lexicon.get("verify").is_none());
    let verification_id = lexicon.get("verification").unwrap();

    let q = query_terms("verification", &lexicon);
    assert_eq!(q.as_slice(), &[verification_id]);
}

#[test]
fn test_query_deduplication_first_occurrence() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "token auth verify",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let q = query_terms("token token verify token auth verify", &lexicon);
    let token_id = lexicon.get("token").unwrap();
    let auth_id = lexicon.get("auth").unwrap();
    let verify_id = lexicon.get("verify").unwrap();

    assert_eq!(q.as_slice(), &[token_id, verify_id, auth_id]);
}

#[test]
fn test_source_multiplicity_retained() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    append_source_terms(
        LexicalField::Body,
        InputKind::Identifier,
        "verify_verify verify",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let verify_id = lexicon.get("verify").unwrap();
    assert_eq!(out.len(), 3);
    for ft in &out {
        assert_eq!(ft.field, LexicalField::Body);
        assert_eq!(ft.term, verify_id);
    }
}

#[test]
fn test_stop_words_affect_query_and_prose_only() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);

    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "is",
        &mut lexicon,
        &mut out,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert!(lexicon.get("is").is_some());

    append_source_terms(
        LexicalField::Attribute,
        InputKind::Identifier,
        "test",
        &mut lexicon,
        &mut out,
    )
    .unwrap();
    assert_eq!(out.len(), 2);
    assert!(lexicon.get("test").is_some());

    append_source_terms(
        LexicalField::Documentation,
        InputKind::Prose,
        "this is a doc with test",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    assert_eq!(out.len(), 4);
    assert_eq!(out[2].field, LexicalField::Documentation);
    assert_eq!(out[2].term, lexicon.get("doc").unwrap());
    assert_eq!(out[3].field, LexicalField::Documentation);
    assert_eq!(out[3].term, lexicon.get("test").unwrap());
}

#[test]
fn test_lexicon_bijection_and_lookup() {
    let lexicon = Lexicon::try_from(vec!["first".to_string(), "second".to_string()]).unwrap();
    assert!(!lexicon.is_empty());
    assert_eq!(lexicon.len(), 2);

    let id0 = lexicon.get("first").unwrap();
    let id1 = lexicon.get("second").unwrap();

    assert_ne!(id0, id1);
    assert_eq!(id0.index(), 0);
    assert_eq!(id1.index(), 1);

    assert_eq!(lexicon.resolve(id0), Some("first"));
    assert_eq!(lexicon.resolve(id1), Some("second"));

    let out_of_bounds: TermId = serde_json::from_str("999").unwrap();
    assert_eq!(lexicon.resolve(out_of_bounds), None);

    assert_eq!(lexicon.get("first"), Some(id0));
    assert_eq!(lexicon.get("second"), Some(id1));
    assert_eq!(lexicon.get("third"), None);

    assert_eq!(lexicon.terms().collect::<Vec<_>>(), ["first", "second"]);
}

#[test]
fn test_lexicon_serde_valid_roundtrip() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "auth token verify",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let json = serde_json::to_string(&lexicon).unwrap();
    assert_eq!(json, "[\"auth\",\"token\",\"verify\"]");

    let deserialized: Lexicon = serde_json::from_str(&json).unwrap();
    assert_eq!(lexicon, deserialized);

    let id0 = deserialized.get("auth").unwrap();
    let id1 = deserialized.get("token").unwrap();
    let id2 = deserialized.get("verify").unwrap();

    assert_eq!(deserialized.resolve(id0), Some("auth"));
    assert_eq!(deserialized.resolve(id1), Some("token"));
    assert_eq!(deserialized.resolve(id2), Some("verify"));

    let postcard_bytes = postcard::to_allocvec(&lexicon).unwrap();
    let postcard_lexicon: Lexicon = postcard::from_bytes(&postcard_bytes).unwrap();
    assert_eq!(lexicon, postcard_lexicon);
}

#[test]
fn test_lexicon_serde_strict_rejection() {
    let empty_term_json = "[\"auth\", \"\"]";
    assert!(serde_json::from_str::<Lexicon>(empty_term_json).is_err());

    let uppercase_json = "[\"Auth\", \"token\"]";
    assert!(serde_json::from_str::<Lexicon>(uppercase_json).is_err());

    let duplicate_json = "[\"auth\", \"token\", \"auth\"]";
    assert!(serde_json::from_str::<Lexicon>(duplicate_json).is_err());

    let separator_json = "[\"auth_token\"]";
    assert!(serde_json::from_str::<Lexicon>(separator_json).is_err());

    let decomposed_json = "[\"E\u{0301}clair\"]";
    assert!(serde_json::from_str::<Lexicon>(decomposed_json).is_err());

    let emoji_json = "[\"foo🙂bar\"]";
    assert!(serde_json::from_str::<Lexicon>(emoji_json).is_err());
}

#[test]
fn test_lexicon_roundtrip_with_multichar_lowercase_expansion() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "İd",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    assert!(lexicon.get("id").is_some());
    for term in lexicon.terms() {
        assert!(is_canonical_term(term), "non-canonical term {term:?}");
    }

    let json = serde_json::to_string(&lexicon).unwrap();
    let roundtrip: Lexicon = serde_json::from_str(&json).unwrap();
    assert_eq!(lexicon, roundtrip);
}

#[test]
fn test_query_stems_handled_to_handle() {
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "handle_error",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let handle_id = lexicon.get("handle").unwrap();
    let q = query_terms("how is the error handled", &lexicon);
    assert!(q.as_slice().contains(&handle_id));
}

#[test]
fn test_warm_ascii_zero_allocation_mechanism() {
    let sample_inputs = [
        "verify_token",
        "XMLHttpRequest2",
        "sha256Digest",
        "AuthService.validate",
        "src/auth/token.rs",
        "r#async_fn",
        "JWTValidator",
        "foo42Bar",
        "utf8Decode",
    ];

    let mut lexicon = Lexicon::new();
    let mut warmup_out = SmallVec::<[FieldTerm; 32]>::with_capacity(64);
    for input in sample_inputs {
        append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            input,
            &mut lexicon,
            &mut warmup_out,
        )
        .unwrap();
    }

    let mut reserved_out = SmallVec::<[FieldTerm; 32]>::with_capacity(64);

    set_alloc_tracking(true);
    reset_alloc_counter();

    for input in sample_inputs {
        append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            input,
            &mut lexicon,
            &mut reserved_out,
        )
        .unwrap();
    }

    let warm_allocations = alloc_count();
    set_alloc_tracking(false);

    assert_eq!(
        warm_allocations, 0,
        "warm ASCII fact appending with reserved capacity must have exactly 0 allocations"
    );

    set_alloc_tracking(true);
    reset_alloc_counter();

    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "cold_unseen_term",
        &mut lexicon,
        &mut reserved_out,
    )
    .unwrap();

    let cold_allocations = alloc_count();
    set_alloc_tracking(false);

    assert!(
        cold_allocations > 0,
        "cold miss insertion must be observed by allocation counter"
    );
}

fn append_fact_defs(defs: &[Def], lexicon: &mut Lexicon, out: &mut SmallVec<[FieldTerm; 32]>) {
    for def in defs {
        append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            &def.name,
            lexicon,
            out,
        )
        .unwrap();

        if let Some(qualified) = def.local_qualified.as_deref() {
            append_source_terms(
                LexicalField::Qualified,
                InputKind::Qualified,
                qualified,
                lexicon,
                out,
            )
            .unwrap();
        }

        for ident in &def.signature_idents {
            append_source_terms(
                LexicalField::Signature,
                InputKind::Identifier,
                ident,
                lexicon,
                out,
            )
            .unwrap();
        }

        for ident in &def.body_idents {
            append_source_terms(
                LexicalField::Body,
                InputKind::Identifier,
                ident,
                lexicon,
                out,
            )
            .unwrap();
        }

        for ident in &def.doc_idents {
            append_source_terms(
                LexicalField::Documentation,
                InputKind::Prose,
                ident,
                lexicon,
                out,
            )
            .unwrap();
        }

        for ident in &def.attribute_idents {
            append_source_terms(
                LexicalField::Attribute,
                InputKind::Identifier,
                ident,
                lexicon,
                out,
            )
            .unwrap();
        }
    }
}

fn append_fact_references(
    references: &[Reference],
    lexicon: &mut Lexicon,
    out: &mut SmallVec<[FieldTerm; 32]>,
) {
    for reference in references {
        let callee_input = reference.qualified.as_deref().unwrap_or(&reference.name);
        append_source_terms(
            LexicalField::Callee,
            if reference.qualified.is_some() {
                InputKind::Qualified
            } else {
                InputKind::Identifier
            },
            callee_input,
            lexicon,
            out,
        )
        .unwrap();
    }
}

fn append_fact_imports(
    imports: &[Import],
    lexicon: &mut Lexicon,
    out: &mut SmallVec<[FieldTerm; 32]>,
) {
    for import in imports {
        if !import.is_glob {
            append_source_terms(
                LexicalField::Import,
                InputKind::Qualified,
                &import.path,
                lexicon,
                out,
            )
            .unwrap();
            if let Some(alias) = import.alias.as_deref() {
                append_source_terms(
                    LexicalField::Import,
                    InputKind::Identifier,
                    alias,
                    lexicon,
                    out,
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn test_issue_04_facts_extraction_handoff_fixture() {
    let source_code = r#"
        use std::collections::HashMap as Map;
        use crate::auth::token::*;

        /// Documenting AuthService token verification.
        #[derive(Debug, Clone)]
        #[serde(rename = "auth")]
        pub struct AuthService {
            pub name: String,
        }

        impl AuthService {
            pub fn verify_token(&self, r#type: &str) -> bool {
                let client = crate::net::HttpClient::new();
                let _ = macro_rules_call!(client);
                true
            }
        }
    "#;

    let mut extractor = RustExtractor::new().unwrap();
    let facts = extractor.extract(source_code.as_bytes()).unwrap();
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(128);

    append_fact_defs(facts.defs(), &mut lexicon, &mut out);
    append_fact_references(facts.references(), &mut lexicon, &mut out);
    append_fact_imports(facts.imports(), &mut lexicon, &mut out);

    assert!(!out.is_empty());
    assert!(lexicon.get("auth").is_some());
    assert!(lexicon.get("service").is_some());
    assert!(lexicon.get("token").is_some());
    assert!(lexicon.get("verify").is_some());
    assert!(lexicon.get("type").is_some());
    assert!(lexicon.get("r").is_none());
}

#[test]
fn test_fuzz_and_regression_seeds() {
    let seeds = [
        "",
        "___",
        "___a___",
        "123",
        "123a456",
        "a1b2c3d4",
        "foo_bar_baz",
        "FooBarBAZ",
        "XMLHttpRequest2345AlphaBetaGamma",
        "r#",
        "r#r#type",
        "r#_foo",
        "Москва_СанктПетербург_HTTP_Client_2026",
        "東京都千代田区丸の内１丁目",
        "👨‍👩‍👧‍👦",
        "foo\u{200D}bar",
        "foo\u{200C}bar",
        "a\u{0300}\u{0301}\u{0302}b",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "11111111111111111111111111111111111111111111111111111111111111111111111111111111",
        "!@#$%^&*()_+-=[]{}|;':\",./<>?",
        "\0\t\n\r ",
    ];

    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(32);

    for seed in seeds {
        let res = append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            seed,
            &mut lexicon,
            &mut out,
        );
        assert!(res.is_ok());

        let q = query_terms(seed, &lexicon);
        let _ = q.as_slice();
    }
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn test_isolated_performance_benchmark() {
    let sample = "AuthService_validate_and_verifyToken_XMLHttpRequest2_sha256Digest";
    let iterations = 100_000usize;

    let start = std::time::Instant::now();
    let mut total_lexemes = 0usize;
    for _ in 0..iterations {
        for_each_lexeme(sample, |_| {
            total_lexemes += 1;
            Ok(())
        })
        .unwrap();
    }
    let elapsed = start.elapsed();
    let nanos_per_op = elapsed.as_nanos() as f64 / iterations as f64;
    let ops_per_sec = (iterations as f64 / elapsed.as_secs_f64()) as usize;
    let bytes_processed = sample.len() * iterations;
    let mb_per_sec = (bytes_processed as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();

    assert!(total_lexemes > 0);
    assert_eq!(total_lexemes, 12 * iterations);
    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::<[FieldTerm; 32]>::with_capacity(32);
    append_source_terms(
        LexicalField::Name,
        InputKind::Identifier,
        "token verification verify auth service validate",
        &mut lexicon,
        &mut out,
    )
    .unwrap();

    let query_iterations = 50_000usize;
    let query_start = std::time::Instant::now();
    let mut query_terms_count = 0usize;
    for _ in 0..query_iterations {
        let q = query_terms("where is token verification handled?", &lexicon);
        query_terms_count += q.len();
    }
    let query_elapsed = query_start.elapsed();
    let query_nanos = query_elapsed.as_nanos() as f64 / query_iterations as f64;
    let query_ops = (query_iterations as f64 / query_elapsed.as_secs_f64()) as usize;

    assert!(query_terms_count > 0);
    assert_eq!(query_terms_count, 3 * query_iterations);
    println!(
        "\n================ ISOLATED PERFORMANCE BENCHMARK ================\n\
         [Tokenizer raw ASCII]   : {nanos_per_op:.2} ns/op | {ops_per_sec} ops/sec | {mb_per_sec:.2} MB/s\n\
         [Query terms pipeline]  : {query_nanos:.2} ns/query | {query_ops} queries/sec\n\
         ================================================================"
    );
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn prop_splitter_never_panics_and_yields_valid_canonical_terms(input in "\\PC*") {
        let mut terms = Vec::new();
        let res = for_each_lexeme(&input, |term| {
            assert!(!term.is_empty(), "emitted lexeme must never be empty");
            terms.push(term.to_string());
            Ok(())
        });
        prop_assert!(res.is_ok());

        for term in &terms {
            prop_assert!(is_canonical_term(term), "term {:?} should be canonical", term);
        }
    }

    #[test]
    fn prop_composed_decomposed_equivalents_match(s in "[a-zA-Z0-9_ -]{1,20}") {
        let composed: String = unicode_normalization::UnicodeNormalization::nfc(s.chars()).collect();
        let decomposed: String = unicode_normalization::UnicodeNormalization::nfd(s.chars()).collect();

        let mut comp_terms = Vec::new();
        for_each_lexeme(&composed, |t| {
            comp_terms.push(t.to_string());
            Ok(())
        }).unwrap();

        let mut decomp_terms = Vec::new();
        for_each_lexeme(&decomposed, |t| {
            decomp_terms.push(t.to_string());
            Ok(())
        }).unwrap();

        prop_assert_eq!(comp_terms, decomp_terms);
    }

    #[test]
    fn prop_repeated_runs_are_identical(s in "\\PC{0,30}") {
        let mut run1 = Vec::new();
        for_each_lexeme(&s, |t| {
            run1.push(t.to_string());
            Ok(())
        }).unwrap();

        let mut run2 = Vec::new();
        for_each_lexeme(&s, |t| {
            run2.push(t.to_string());
            Ok(())
        }).unwrap();

        prop_assert_eq!(run1, run2);
    }
}

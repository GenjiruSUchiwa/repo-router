#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rr_core::lex::split::for_each_lexeme;
use rr_core::lex::{append_source_terms, query_terms, InputKind, LexicalField, Lexicon};
use smallvec::SmallVec;

fn bench_split_raw(c: &mut Criterion) {
    let ascii_text = "AuthService_validate_and_verifyToken_XMLHttpRequest2_sha256Digest";
    let bytes_len = u64::try_from(ascii_text.len()).unwrap();

    let mut group = c.benchmark_group("lex_split_raw");
    group.throughput(Throughput::Bytes(bytes_len));

    group.bench_function("ascii_tokenization", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for_each_lexeme(black_box(ascii_text), |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
            count
        });
    });

    let unicode_text = "Москва_HTTPClient_東京駅_Service_E\u{0301}clair_ΣParser";
    let unicode_bytes_len = u64::try_from(unicode_text.len()).unwrap();
    group.throughput(Throughput::Bytes(unicode_bytes_len));
    group.bench_function("unicode_tokenization", |b| {
        b.iter(|| {
            let mut count = 0usize;
            for_each_lexeme(black_box(unicode_text), |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
            count
        });
    });

    group.finish();
}

fn bench_lex_ascii(c: &mut Criterion) {
    let ascii_samples = [
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
    let mut out = SmallVec::with_capacity(32);
    for sample in ascii_samples {
        append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            sample,
            &mut lexicon,
            &mut out,
        )
        .unwrap();
    }

    let mut group = c.benchmark_group("lex_ascii");

    group.bench_function("append_source_terms_warm", |b| {
        b.iter(|| {
            let mut buf = SmallVec::<[_; 32]>::new();
            for sample in ascii_samples {
                append_source_terms(
                    LexicalField::Name,
                    InputKind::Identifier,
                    black_box(sample),
                    &mut lexicon,
                    &mut buf,
                )
                .unwrap();
            }
            buf
        });
    });

    group.bench_function("query_terms_warm", |b| {
        b.iter(|| query_terms(black_box("where is token verification handled?"), &lexicon));
    });

    group.finish();
}

fn bench_lex_cold_interning(c: &mut Criterion) {
    let unique_identifiers: Vec<String> = (0..500)
        .map(|i| format!("generated_symbol_name_{i}_field_term"))
        .collect();

    let mut group = c.benchmark_group("lex_cold");

    group.bench_function(BenchmarkId::new("cold_intern_batch", 500), |b| {
        b.iter(|| {
            let mut lexicon = Lexicon::new();
            let mut buf = SmallVec::<[_; 32]>::new();
            for ident in &unique_identifiers {
                append_source_terms(
                    LexicalField::Name,
                    InputKind::Identifier,
                    black_box(ident),
                    &mut lexicon,
                    &mut buf,
                )
                .unwrap();
            }
            lexicon
        });
    });

    group.finish();
}

fn bench_lex_unicode(c: &mut Criterion) {
    let unicode_samples = [
        "МоскваHTTPClient",
        "東京駅_Service",
        "E\u{0301}clairValidation",
        "ΣParserDefinition",
        "héllo_wörld_test",
    ];

    let mut lexicon = Lexicon::new();
    let mut out = SmallVec::with_capacity(32);
    for sample in unicode_samples {
        append_source_terms(
            LexicalField::Name,
            InputKind::Identifier,
            sample,
            &mut lexicon,
            &mut out,
        )
        .unwrap();
    }

    let mut group = c.benchmark_group("lex_unicode");

    group.bench_function("append_source_terms_warm", |b| {
        b.iter(|| {
            let mut buf = SmallVec::<[_; 32]>::new();
            for sample in unicode_samples {
                append_source_terms(
                    LexicalField::Name,
                    InputKind::Identifier,
                    black_box(sample),
                    &mut lexicon,
                    &mut buf,
                )
                .unwrap();
            }
            buf
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_split_raw,
    bench_lex_ascii,
    bench_lex_cold_interning,
    bench_lex_unicode
);
criterion_main!(benches);

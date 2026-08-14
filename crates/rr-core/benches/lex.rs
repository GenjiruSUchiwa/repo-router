#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rr_core::lex::{append_source_terms, query_terms, InputKind, LexicalField, Lexicon};
use smallvec::SmallVec;

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

    c.bench_function("lex_append_source_terms_ascii_warm", |b| {
        b.iter(|| {
            let mut buf = SmallVec::with_capacity(32);
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

    c.bench_function("lex_query_terms_ascii_warm", |b| {
        b.iter(|| query_terms(black_box("where is token verification handled?"), &lexicon));
    });
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

    c.bench_function("lex_append_source_terms_unicode_warm", |b| {
        b.iter(|| {
            let mut buf = SmallVec::with_capacity(32);
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
}

criterion_group!(benches, bench_lex_ascii, bench_lex_unicode);
criterion_main!(benches);

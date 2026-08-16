#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rr_core::parser::RustExtractor;
use rr_core::ParseStatus;

fn bench_extract(c: &mut Criterion) {
    let content = include_bytes!("fixtures/rust-300.rs");
    let mut extractor = RustExtractor::new().unwrap();
    let facts = extractor.extract(content).unwrap();
    assert!(
        matches!(
            facts.status(),
            ParseStatus::Complete | ParseStatus::Recovered { .. } | ParseStatus::Tags { .. }
        ),
        "benchmark fixture must not degrade"
    );
    assert!(!facts.defs().is_empty());

    c.bench_function("rust_extract_300_lines", |b| {
        b.iter(|| extractor.extract(black_box(content)));
    });
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);

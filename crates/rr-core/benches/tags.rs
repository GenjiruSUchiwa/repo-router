#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rr_core::parser::Registry;
use rr_core::{Lang, ParseStatus};

fn bench_python_tags(c: &mut Criterion) {
    let content = include_bytes!("fixtures/python-300.py");
    let mut registry = Registry::new();
    let extractor = registry.for_lang(Lang::Python).unwrap().unwrap();
    let facts = extractor.extract(content).unwrap();
    assert!(matches!(facts.status(), ParseStatus::Tags { .. }));
    assert!(!facts.defs().is_empty());

    c.bench_function("python_tags_300_lines", |b| {
        b.iter(|| extractor.extract(black_box(content)));
    });
}

criterion_group!(benches, bench_python_tags);
criterion_main!(benches);

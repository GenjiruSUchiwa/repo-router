#![allow(clippy::unwrap_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rr_core::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::RustExtractor;
use rr_core::path::RelPath;
use rr_core::query::{finish_exact, parse_query, route_exact, QueryRequest};
use rr_core::render::{render_json, render_text};

fn setup_benchmark_snapshot() -> rr_core::index::Snapshot {
    let mut inputs = Vec::new();
    let mut extractor = RustExtractor::new().unwrap();

    let auth_code =
        b"pub fn verify_token(token: &str) -> bool { true }\npub fn decode_token() {}\n";
    let auth_facts = extractor.extract(auth_code).unwrap();
    inputs.push(FileInput {
        path: RelPath::new("src/auth/token.rs").unwrap(),
        oid: Oid::from_raw(&[1; 32]).unwrap(),
        representation: ContentRepresentation::RawNoGit,
        generated: false,
        language: Lang::Rust,
        parse_status: rr_core::ParseStatus::Complete,
        facts: auth_facts,
    });

    for i in 0..10 {
        let code =
            format!("pub fn session() {{}}\npub struct Session;\npub fn handle_{i}() {{}}\n");
        let facts = extractor.extract(code.as_bytes()).unwrap();
        inputs.push(FileInput {
            path: RelPath::new(format!("src/module_{i}/session.rs")).unwrap(),
            oid: Oid::from_raw(&[u8::try_from(i + 2).unwrap(); 32]).unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: rr_core::ParseStatus::Complete,
            facts,
        });
    }

    let meta = SnapshotMeta::new(None, true, [0; 32]);
    let (snapshot, _) = SnapshotBuilder::new(meta).build(inputs).unwrap();
    snapshot
}

fn bench_query_exact(c: &mut Criterion) {
    let snapshot = setup_benchmark_snapshot();
    let mut group = c.benchmark_group("query_exact");

    group.bench_function("unique_direct", |b| {
        b.iter(|| {
            let request = QueryRequest::new(black_box("verify_token"), None);
            let parsed = parse_query(&snapshot, request).unwrap();
            let outcome = route_exact(&snapshot, &parsed);
            let result = finish_exact(outcome);
            black_box(result);
        });
    });

    group.bench_function("ambiguous_candidates", |b| {
        b.iter(|| {
            let request = QueryRequest::new(black_box("session"), None);
            let parsed = parse_query(&snapshot, request).unwrap();
            let outcome = route_exact(&snapshot, &parsed);
            let result = finish_exact(outcome);
            black_box(result);
        });
    });

    group.bench_function("miss", |b| {
        b.iter(|| {
            let request = QueryRequest::new(black_box("nonexistent_symbol_lookup"), None);
            let parsed = parse_query(&snapshot, request).unwrap();
            let outcome = route_exact(&snapshot, &parsed);
            let result = finish_exact(outcome);
            black_box(result);
        });
    });

    group.bench_function("render_text_direct", |b| {
        let request = QueryRequest::new("verify_token", None);
        let parsed = parse_query(&snapshot, request).unwrap();
        let outcome = route_exact(&snapshot, &parsed);
        let result = finish_exact(outcome);
        b.iter(|| {
            let rendered = render_text(&snapshot, black_box(&result)).unwrap();
            black_box(rendered);
        });
    });

    group.bench_function("render_json_direct", |b| {
        let request = QueryRequest::new("verify_token", None);
        let parsed = parse_query(&snapshot, request).unwrap();
        let outcome = route_exact(&snapshot, &parsed);
        let result = finish_exact(outcome);
        b.iter(|| {
            let rendered = render_json(&snapshot, black_box(&result)).unwrap();
            black_box(rendered);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_query_exact);
criterion_main!(benches);

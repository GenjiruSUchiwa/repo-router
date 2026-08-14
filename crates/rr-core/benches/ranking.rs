//! Ranking cost on a ten-thousand-symbol snapshot.
//!
//! The queries span the shapes whose costs differ: one rare term, a mixture of
//! rare and common terms, a term every symbol carries, a term whose union
//! overflows the candidate cap, and a long query holding eight terms.
//!
//! Throughput is measured in posting entries per second, which is the work the
//! merge actually does; a query answered from a rare term is fast because it
//! scans few postings, not because it scores differently. The table printed
//! before the timings reports, per query, the postings scanned, the union
//! members the cap dropped, and the allocations the warm ranker makes.
//!
//! `arbitrary_cut` is the column to read next to `dropped`: dropping thousands
//! of clearly weaker members is the bounded design working, while a cut through
//! members the retention key ranks equal means the surviving set was settled on
//! symbol id, and a discarded member could have outscored the answer.

#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::hint::black_box;

#[path = "../tests/support/counting_alloc.rs"]
mod counting_alloc;

use counting_alloc::{measure, CountingAllocator};
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use rr_core::index::{ContentRepresentation, FileInput, Snapshot, SnapshotBuilder, SnapshotMeta};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::RustExtractor;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, QueryRequest};
use rr_core::ranking::{rank, route_lexical, RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::ParseStatus;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const FILES: usize = 200;
const SYMBOLS_PER_FILE: usize = 50;
const VERBS: [&str; 10] = [
    "handle", "verify", "decode", "encode", "resolve", "collect", "insert", "remove", "render",
    "dispatch",
];
const NOUNS: [&str; 10] = [
    "token", "session", "cache", "entry", "route", "event", "packet", "record", "buffer", "policy",
];
const QUERIES: [(&str, &str); 5] = [
    ("rare_term", "singleton beacon probe"),
    ("mixed_terms", "verify the session token cache"),
    ("ubiquitous_term", "value"),
    ("cap_overflow", "handle event"),
    (
        "eight_terms",
        "beacon probe handle event token session cache policy",
    ),
];

fn generated_source(file: usize) -> String {
    let mut code = String::from("//! Generated module for the ranking benchmark.\n\n");
    for index in 0..SYMBOLS_PER_FILE {
        let verb = VERBS[(file + index) % VERBS.len()];
        let noun = NOUNS[(file * 7 + index) % NOUNS.len()];
        write!(
            code,
            "/// Records how the {verb} step touches the {noun} value.\n\
             pub fn {verb}_{noun}_{file:03}_{index:02}(value: u32) -> u32 {{\n    \
             value + {index}\n}}\n\n"
        )
        .unwrap();
    }
    if file == 0 {
        code.push_str(
            "/// The only definition naming the beacon.\n\
             pub fn singleton_beacon_probe(value: u32) -> u32 {\n    value\n}\n",
        );
    }
    code
}

fn benchmark_snapshot() -> Snapshot {
    let mut extractor = RustExtractor::new().unwrap();
    let mut inputs = Vec::with_capacity(FILES);
    for file in 0..FILES {
        let code = generated_source(file);
        let facts = extractor.extract(code.as_bytes()).unwrap();
        inputs.push(FileInput {
            path: RelPath::new(format!("src/module_{file:03}/generated.rs")).unwrap(),
            oid: Oid::from_raw(blake3::hash(code.as_bytes()).as_bytes()).unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: ParseStatus::Complete,
            facts,
        });
    }
    let (snapshot, _counts) = SnapshotBuilder::new(SnapshotMeta::new(None, true))
        .build(inputs)
        .unwrap();
    assert!(
        snapshot.symbols.len() >= 10_000,
        "the benchmark snapshot holds {} symbols",
        snapshot.symbols.len()
    );
    snapshot
}

/// What one query costs, measured once on a warm scratch.
struct Cost {
    postings_scanned: u64,
    candidates_before_cap: u64,
    candidates_dropped: u64,
    candidates_scored: u16,
    cap_cut_a_tie: bool,
    warm_allocations: u64,
}

fn measure_cost(snapshot: &Snapshot, text: &str) -> Cost {
    let parsed = parse_query(snapshot, QueryRequest::new(text, None)).unwrap();
    let mut scratch = RankingScratch::new();
    let _warm = rank(
        snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    let (evidence, warm_allocations) = measure(|| {
        let (_ranked, evidence) = rank(
            snapshot,
            &parsed.terms,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .unwrap();
        evidence
    });
    Cost {
        postings_scanned: evidence.posting_hits_scanned,
        candidates_before_cap: evidence.candidates_before_cap,
        candidates_dropped: evidence.candidates_dropped,
        candidates_scored: evidence.candidates_scored,
        cap_cut_a_tie: evidence.cap_cut_a_tie,
        warm_allocations,
    }
}

fn report_costs(snapshot: &Snapshot, costs: &[(&str, Cost)]) {
    println!(
        "\nranking benchmark corpus: {} symbols in {} files, {} terms",
        snapshot.symbols.len(),
        snapshot.files.len(),
        snapshot.terms.len()
    );
    println!(
        "{:<16} {:>9} {:>10} {:>8} {:>7} {:>13} {:>7}",
        "query", "postings", "union", "dropped", "scored", "arbitrary_cut", "allocs"
    );
    for (name, cost) in costs {
        println!(
            "{:<16} {:>9} {:>10} {:>8} {:>7} {:>13} {:>7}",
            name,
            cost.postings_scanned,
            cost.candidates_before_cap,
            cost.candidates_dropped,
            cost.candidates_scored,
            if cost.cap_cut_a_tie { "yes" } else { "no" },
            cost.warm_allocations
        );
    }
    println!();
}

fn bench_ranking(criterion: &mut Criterion) {
    let snapshot = benchmark_snapshot();
    let costs: Vec<(&str, Cost)> = QUERIES
        .iter()
        .map(|(name, text)| (*name, measure_cost(&snapshot, text)))
        .collect();
    report_costs(&snapshot, &costs);

    let mut group = criterion.benchmark_group("ranking");
    for ((name, text), (_, cost)) in QUERIES.iter().zip(&costs) {
        let parsed = parse_query(&snapshot, QueryRequest::new(text, None)).unwrap();
        let mut scratch = RankingScratch::new();
        let _warm = rank(
            &snapshot,
            &parsed.terms,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .unwrap();
        group.throughput(Throughput::Elements(cost.postings_scanned.max(1)));
        group.bench_function(*name, |bencher| {
            bencher.iter(|| {
                let (ranked, evidence) = rank(
                    &snapshot,
                    black_box(&parsed.terms),
                    &DEFAULT_RANKING_PROFILE,
                    &mut scratch,
                )
                .unwrap();
                black_box((ranked.len(), evidence));
            });
        });
    }

    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("verify the session token cache", None),
    )
    .unwrap();
    let mut scratch = RankingScratch::new();
    group.throughput(Throughput::Elements(1));
    group.bench_function("route_lexical", |bencher| {
        bencher.iter(|| {
            let (result, evidence) = route_lexical(
                &snapshot,
                black_box(&parsed.terms),
                &DEFAULT_RANKING_PROFILE,
                &mut scratch,
            )
            .unwrap();
            black_box((result, evidence));
        });
    });
    group.finish();
}

criterion_group!(benches, bench_ranking);
criterion_main!(benches);

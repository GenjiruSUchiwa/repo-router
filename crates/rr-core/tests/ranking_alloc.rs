//! Proves the ranker reaches an allocation-free steady state.

#![allow(clippy::unwrap_used)]

mod support;

#[path = "support/counting_alloc.rs"]
mod counting_alloc;

use counting_alloc::{measure, CountingAllocator};
use rr_core::query::{parse_query, ParsedQuery, QueryRequest};
use rr_core::ranking::{rank, RankingScratch, DEFAULT_RANKING_PROFILE};
use support::fixture_snapshot;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Queries chosen to cover the widest scratch the ranker will ever need here:
/// the deepest union, the most streams, and the emptiest result.
const QUERIES: [&str; 5] = [
    "how is an event handled?",
    "event dispatch entry point",
    "record an event in the audit log",
    "src",
    "quantum flux capacitor",
];

#[test]
fn ranking_alloc_reaches_an_allocation_free_steady_state() {
    let snapshot = fixture_snapshot("wide");
    let parsed: Vec<ParsedQuery<'_>> = QUERIES
        .iter()
        .map(|query| parse_query(&snapshot, QueryRequest::new(query, None)).unwrap())
        .collect();
    let mut scratch = RankingScratch::new();

    for query in &parsed {
        let (ranked, _evidence) = rank(
            &snapshot,
            &query.terms,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .unwrap();
        assert!(ranked.len() <= usize::from(DEFAULT_RANKING_PROFILE.candidate_limit));
    }

    let (scored, allocated) = measure(|| {
        let mut scored = 0usize;
        for query in &parsed {
            let (ranked, _evidence) = rank(
                &snapshot,
                &query.terms,
                &DEFAULT_RANKING_PROFILE,
                &mut scratch,
            )
            .unwrap();
            scored += ranked.len();
        }
        scored
    });

    assert!(scored > 0, "the measured pass must rank something");
    assert_eq!(
        allocated, 0,
        "ranking allocated {allocated} times after its scratch was warm"
    );
}

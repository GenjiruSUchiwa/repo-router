//! Properties the lexical ranker holds for every query and every threshold.

#![allow(clippy::unwrap_used)]

mod support;

use std::sync::OnceLock;

use proptest::prelude::*;
use rr_core::index::Snapshot;
use rr_core::query::{parse_query, QueryRequest};
use rr_core::ranking::{
    decide, rank, DecisionThresholds, MarginPpm, RankedSymbol, RankingScratch, Score,
    CANDIDATE_LIMIT, DEFAULT_RANKING_PROFILE, MARGIN_SCALE,
};
use rr_core::result::{NoneReason, QueryResult, TargetId};
use support::{fixture_snapshot, FIXTURE_REPOSITORIES};

/// Words the fixtures define, words they only mention, and words they never saw.
const VOCABULARY: [&str; 32] = [
    "token",
    "verification",
    "decode",
    "session",
    "claims",
    "password",
    "hash",
    "entry",
    "cache",
    "evict",
    "key",
    "value",
    "serialize",
    "route",
    "pattern",
    "table",
    "middleware",
    "request",
    "retry",
    "backoff",
    "delay",
    "event",
    "handled",
    "dispatch",
    "record",
    "circle",
    "area",
    "radius",
    "vector",
    "src",
    "quantum",
    "capacitor",
];

fn snapshots() -> &'static [Snapshot; FIXTURE_REPOSITORIES.len()] {
    static SNAPSHOTS: OnceLock<[Snapshot; FIXTURE_REPOSITORIES.len()]> = OnceLock::new();
    SNAPSHOTS.get_or_init(|| FIXTURE_REPOSITORIES.map(fixture_snapshot))
}

/// Ranks one word list and copies the result out of the scratch.
fn ranked_of(snapshot: &Snapshot, words: &[&str]) -> Vec<RankedSymbol> {
    let mut scratch = RankingScratch::new();
    let query = words.join(" ");
    let parsed = parse_query(snapshot, QueryRequest::new(&query, None)).unwrap();
    let (ranked, evidence) = rank(
        snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    assert_eq!(usize::from(evidence.candidates_scored), ranked.len());
    ranked.to_vec()
}

fn words_of(indices: &[usize]) -> Vec<&'static str> {
    indices.iter().map(|index| VOCABULARY[*index]).collect()
}

fn query_strategy() -> impl Strategy<Value = (usize, Vec<usize>)> {
    (
        0..FIXTURE_REPOSITORIES.len(),
        prop::collection::vec(0..VOCABULARY.len(), 1..6),
    )
}

fn thresholds_strategy() -> impl Strategy<Value = DecisionThresholds> {
    (0u64..60_000_000, 0u64..60_000_000, 0u32..=MARGIN_SCALE).prop_map(|(first, second, margin)| {
        DecisionThresholds {
            none_below: Score::from_millionths(first.min(second)),
            direct_at_least: Score::from_millionths(first.max(second)),
            direct_margin_at_least: MarginPpm::new(margin).unwrap(),
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Scoring sums in term-id order, so a permuted question ranks identically.
    #[test]
    fn ranking_properties_ignore_query_word_order((repository, indices) in query_strategy()) {
        let snapshot = &snapshots()[repository];
        let words = words_of(&indices);
        let mut reversed = words.clone();
        reversed.reverse();

        prop_assert_eq!(ranked_of(snapshot, &words), ranked_of(snapshot, &reversed));
    }

    /// The result is a bounded, strictly ordered, duplicate-free ranking.
    #[test]
    fn ranking_properties_bound_and_order_the_candidates(
        (repository, indices) in query_strategy(),
    ) {
        let snapshot = &snapshots()[repository];
        let ranked = ranked_of(snapshot, &words_of(&indices));

        prop_assert!(ranked.len() <= CANDIDATE_LIMIT);
        for pair in ranked.windows(2) {
            prop_assert!(
                pair[0].score > pair[1].score
                    || (pair[0].score == pair[1].score && pair[0].symbol < pair[1].symbol),
                "ranking is ordered by score then symbol id"
            );
        }
    }

    /// A reused scratch never leaks state from the query before it.
    #[test]
    fn ranking_properties_are_independent_of_the_scratch(
        (repository, first) in query_strategy(),
        (_, second) in query_strategy(),
    ) {
        let snapshot = &snapshots()[repository];
        let mut shared = RankingScratch::new();
        let query = words_of(&first).join(" ");
        let parsed = parse_query(snapshot, QueryRequest::new(&query, None)).unwrap();

        let _ = rank(snapshot, &parsed.terms, &DEFAULT_RANKING_PROFILE, &mut shared).unwrap();
        let other = words_of(&second).join(" ");
        let parsed_other = parse_query(snapshot, QueryRequest::new(&other, None)).unwrap();
        let (ranked, _) =
            rank(snapshot, &parsed_other.terms, &DEFAULT_RANKING_PROFILE, &mut shared).unwrap();

        prop_assert_eq!(ranked.to_vec(), ranked_of(snapshot, &words_of(&second)));
    }

    /// Every field contributes a non-negative amount, so asking for more words
    /// never lowers the score of a symbol that survives both rankings.
    #[test]
    fn ranking_properties_never_lower_a_surviving_score(
        (repository, indices) in query_strategy(),
        extra in 0..VOCABULARY.len(),
    ) {
        let snapshot = &snapshots()[repository];
        let words = words_of(&indices);
        let mut widened = words.clone();
        widened.push(VOCABULARY[extra]);

        let before = ranked_of(snapshot, &words);
        let after = ranked_of(snapshot, &widened);
        for entry in &before {
            if let Some(widened) = after.iter().find(|other| other.symbol == entry.symbol) {
                prop_assert!(
                    widened.score >= entry.score,
                    "adding a word lowered a score from {} to {}",
                    entry.score,
                    widened.score
                );
            }
        }
    }

    /// The decision never contradicts the thresholds that produced it.
    #[test]
    fn ranking_properties_decide_respects_its_thresholds(
        (repository, indices) in query_strategy(),
        thresholds in thresholds_strategy(),
    ) {
        let snapshot = &snapshots()[repository];
        let ranked = ranked_of(snapshot, &words_of(&indices));
        let mut profile = DEFAULT_RANKING_PROFILE;
        profile.thresholds = thresholds;
        let top = ranked.first().map_or(Score::ZERO, |entry| entry.score);
        let runner_up = ranked.get(1).map_or(Score::ZERO, |entry| entry.score);
        let margin = MarginPpm::between(top, runner_up);

        match decide(&ranked, &profile).unwrap() {
            QueryResult::Direct { candidate, .. } => {
                prop_assert!(top >= thresholds.direct_at_least);
                prop_assert!(top >= thresholds.none_below);
                prop_assert!(margin >= thresholds.direct_margin_at_least);
                prop_assert_eq!(candidate.target, TargetId::Symbol(ranked[0].symbol));
            }
            QueryResult::Candidates { candidates, .. } => {
                prop_assert!(!ranked.is_empty());
                prop_assert!(candidates.len() <= usize::from(profile.result_limit));
                prop_assert_eq!(candidates.len(), ranked.len().min(usize::from(profile.result_limit)));
                prop_assert!(top >= thresholds.none_below);
            }
            QueryResult::None { reason, .. } => match reason {
                NoneReason::NotFound => prop_assert!(ranked.is_empty()),
                NoneReason::LowConfidence => prop_assert!(top < thresholds.none_below),
            },
        }
    }

    /// Fixed-point scores round-trip, and margins stay inside their scale.
    #[test]
    fn ranking_properties_quantize_scores_and_margins(top in 0u64..u64::MAX, runner_up in 0u64..u64::MAX) {
        let top = Score::from_millionths(top);
        let runner_up = Score::from_millionths(runner_up.min(top.millionths()));
        prop_assert_eq!(Score::from_millionths(top.millionths()), top);

        let margin = MarginPpm::between(top, runner_up);
        prop_assert!(margin.get() <= MARGIN_SCALE);
        prop_assert_eq!(MarginPpm::between(top, top), MarginPpm::ZERO);
        if top.millionths() > 0 {
            prop_assert_eq!(MarginPpm::between(top, Score::ZERO), MarginPpm::FULL);
        }
    }

    /// A closer runner-up never widens the margin.
    #[test]
    fn ranking_properties_margins_shrink_as_the_runner_up_rises(
        top in 1u64..u64::MAX,
        lower in 0u64..u64::MAX,
        higher in 0u64..u64::MAX,
    ) {
        let top = Score::from_millionths(top);
        let lower = Score::from_millionths(lower.min(top.millionths()));
        let higher = Score::from_millionths(higher.min(top.millionths()));
        let (lower, higher) = if lower <= higher { (lower, higher) } else { (higher, lower) };

        prop_assert!(MarginPpm::between(top, lower) >= MarginPpm::between(top, higher));
    }
}

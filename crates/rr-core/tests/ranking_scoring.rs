//! Scoring arithmetic, field coverage, and determinism.
//!
//! These suites work on tiny hand-written snapshots rather than the fixture
//! repositories, so every number a test asserts can be derived from the scoring
//! formula by hand instead of recorded from a previous run.
//!
//! Most of them score through a profile that leaves every field but one
//! unscored. Isolating a field is what makes a claim about that field provable:
//! a total over ten fields can hide a field that contributes nothing.

#![allow(clippy::unwrap_used)]

mod support;

use rr_core::index::Snapshot;
use rr_core::lex::LexicalField;
use rr_core::query::{parse_query, route_query, QueryRequest};
use rr_core::ranking::{
    decide, rank, DecisionThresholds, FieldParams, MarginPpm, RankingError, RankingProfile,
    RankingScratch, RankingStamp, Score, DEFAULT_RANKING_PROFILE,
};
use rr_core::result::{NoneReason, Pipeline, QueryResult};
use support::{fixture_snapshot, synthetic_snapshot};

/// Restates the documented inverse document frequency for a field where
/// `documents` symbols are populated and `containing` of them carry the term.
fn idf(documents: u32, containing: u32) -> f64 {
    let documents = f64::from(documents);
    let containing = f64::from(containing);
    (1.0 + (documents - containing + 0.5) / (containing + 0.5)).ln()
}

/// Quantizes a hand-derived raw score the way the ranker does.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn quantized(raw: f64) -> Score {
    Score::from_millionths((raw * 1_000_000.0 + 0.5).floor() as u64)
}

/// Builds a profile that scores exactly one field.
fn single_field(field: LexicalField, params: FieldParams) -> RankingProfile {
    let mut profile = RankingProfile {
        fields: [FieldParams::UNSCORED; LexicalField::COUNT],
        thresholds: DecisionThresholds {
            none_below: Score::ZERO,
            direct_at_least: Score::ZERO,
            direct_margin_at_least: MarginPpm::ZERO,
        },
        ..DEFAULT_RANKING_PROFILE
    };
    profile.fields[field.index()] = params;
    profile.validate().unwrap();
    profile
}

/// Ranks a query under a profile the snapshot was not built with.
///
/// Scoring parameters are stamped into the snapshot, so a test that changes
/// them has to restamp; that check is the reason a shipped snapshot can never
/// be read through the wrong profile.
fn top_score(snapshot: &mut Snapshot, query: &str, profile: &RankingProfile) -> Option<Score> {
    snapshot.meta.ranking = RankingStamp::of(profile);
    let parsed = parse_query(snapshot, QueryRequest::new(query, None)).unwrap();
    let mut scratch = RankingScratch::new();
    let (ranked, _evidence) = rank(snapshot, &parsed.terms, profile, &mut scratch).unwrap();
    ranked.first().map(|entry| entry.score)
}

#[test]
fn ranking_scoring_matches_a_hand_calculated_bm25_sum() {
    let snapshot = synthetic_snapshot(&[("src/a.rs", "pub fn alpha() {}\n")]);
    assert_eq!(snapshot.symbols.len(), 1, "the corpus holds one symbol");
    let lengths = snapshot.symbols[0].field_lengths;
    assert_eq!(lengths.get(LexicalField::Name), 1, "the name is one term");

    let mut scratch = RankingScratch::new();
    let parsed = parse_query(&snapshot, QueryRequest::new("alpha", None)).unwrap();
    let (ranked, _evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();

    let name = DEFAULT_RANKING_PROFILE.field(LexicalField::Name);
    let qualified = DEFAULT_RANKING_PROFILE.field(LexicalField::Qualified);
    let expected = quantized((name.boost + qualified.boost) * idf(1, 1));
    assert_eq!(
        expected,
        Score::from_millionths(3_739_867),
        "13 * ln(4/3) rounds to 3.739867"
    );
    assert_eq!(
        ranked[0].score, expected,
        "`alpha` appears in the name and the qualification of the only symbol; every field \
         length equals its own corpus average, so each saturated term frequency is exactly one \
         and the score is the sum of the two boosts times the shared inverse document frequency"
    );
}

#[test]
fn ranking_scoring_reads_every_declared_field() {
    let mut snapshot = synthetic_snapshot(&[(
        "src/zeta_module.rs",
        "//! Module documentation.\n\
         use crate::support::zeta_helper;\n\
         \n\
         /// Zeta explains what this does.\n\
         #[zeta_marker]\n\
         pub fn zeta_probe(input: ZetaInput) -> ZetaInput {\n    \
         zeta_helper(input)\n\
         }\n\
         \n\
         /// Calls the probe.\n\
         pub fn zeta_caller() {\n    \
         zeta_probe(ZetaInput);\n\
         }\n",
    )]);

    for field in LexicalField::ALL {
        let profile = single_field(field, FieldParams::new(1.0, 1.2, 0.5));
        let score = top_score(&mut snapshot, "zeta", &profile);
        assert!(
            score.is_some_and(|score| score > Score::ZERO),
            "no symbol carries `zeta` in the {field:?} field, so that field cannot influence \
             any answer: {score:?}"
        );
    }
}

#[test]
fn ranking_scoring_penalizes_length_only_where_b_is_positive() {
    let mut snapshot = synthetic_snapshot(&[
        (
            "src/short.rs",
            "/// Beacon.\npub fn short_beacon_reader() {}\n",
        ),
        (
            "src/long.rs",
            "/// Beacon described at length with many additional explanatory words that \
             stretch this documentation field well beyond the length of its neighbour.\n\
             pub fn long_beacon_reader() {}\n",
        ),
    ]);

    let unnormalized = single_field(LexicalField::Documentation, FieldParams::new(1.0, 1.2, 0.0));
    let flat = top_score(&mut snapshot, "beacon", &unnormalized).unwrap();

    let normalized = single_field(
        LexicalField::Documentation,
        FieldParams::new(1.0, 1.2, 0.75),
    );
    let penalized = top_score(&mut snapshot, "beacon", &normalized).unwrap();

    assert_eq!(
        flat,
        quantized(idf(2, 2)),
        "with b = 0 both documents saturate identically, so the top score is one boost times \
         the inverse document frequency of a term in both documents"
    );
    assert!(
        penalized > flat,
        "with b > 0 the shorter documentation is normalized above the corpus average, so it \
         must outscore what no normalization gives: {penalized} <= {flat}"
    );
}

#[test]
fn ranking_scoring_saturates_term_frequency() {
    let profile = single_field(LexicalField::Body, FieldParams::new(1.0, 1.2, 0.0));
    let mut previous = Score::ZERO;
    let mut increments = Vec::new();
    for repeats in 1..=6usize {
        let calls = "    marker(marker);\n".repeat(repeats);
        let code = format!("pub fn holder() {{\n{calls}}}\n");
        let mut snapshot = synthetic_snapshot(&[("src/body.rs", code.as_str())]);
        let score = top_score(&mut snapshot, "marker", &profile).unwrap();
        assert!(
            score > previous,
            "more occurrences must never score lower: {score} <= {previous}"
        );
        increments.push(score.millionths() - previous.millionths());
        previous = score;
    }

    assert!(
        increments.windows(2).all(|pair| pair[0] > pair[1]),
        "each further occurrence must add less than the one before it: {increments:?}"
    );
    let ceiling = quantized(idf(1, 1) * (1.2 + 1.0));
    assert!(
        previous < ceiling,
        "saturation bounds the field at boost * idf * (k1 + 1): {previous} >= {ceiling}"
    );
}

#[test]
fn ranking_scoring_survives_a_field_no_symbol_populates() {
    let snapshot = synthetic_snapshot(&[("src/bare.rs", "pub fn bare_marker() {}\n")]);
    let empty: Vec<LexicalField> = LexicalField::ALL
        .into_iter()
        .filter(|field| snapshot.corpus.field(*field).document_count == 0)
        .collect();
    assert!(
        !empty.is_empty(),
        "the regression needs a corpus that leaves at least one field unpopulated"
    );

    let mut scratch = RankingScratch::new();
    let parsed = parse_query(&snapshot, QueryRequest::new("bare marker", None)).unwrap();
    let (ranked, _evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    assert_eq!(
        ranked.len(),
        1,
        "an unpopulated field owns no posting list, so it must leave the query alone rather \
         than fail it for having no average length: {empty:?}"
    );
}

#[test]
fn ranking_decision_boundaries_are_exactly_inclusive() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how do we create a session?", None),
    )
    .unwrap();
    let (ranked, _evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    let top = ranked[0].score;
    let margin = MarginPpm::between(top, ranked[1].score);

    let exactly = RankingProfile {
        thresholds: DecisionThresholds {
            none_below: top,
            direct_at_least: top,
            direct_margin_at_least: margin,
        },
        ..DEFAULT_RANKING_PROFILE
    };
    assert!(matches!(
        decide(ranked, &exactly).unwrap(),
        QueryResult::Direct { .. }
    ));

    let over_score = RankingProfile {
        thresholds: DecisionThresholds {
            direct_at_least: Score::from_millionths(top.millionths() + 1),
            ..exactly.thresholds
        },
        ..DEFAULT_RANKING_PROFILE
    };
    assert!(
        matches!(
            decide(ranked, &over_score).unwrap(),
            QueryResult::Candidates { .. }
        ),
        "the direct score floor is inclusive, so one millionth above the top score withholds \
         the direct answer"
    );

    let over_margin = RankingProfile {
        thresholds: DecisionThresholds {
            direct_margin_at_least: MarginPpm::new(margin.get() + 1).unwrap(),
            ..exactly.thresholds
        },
        ..DEFAULT_RANKING_PROFILE
    };
    assert!(matches!(
        decide(ranked, &over_margin).unwrap(),
        QueryResult::Candidates { .. }
    ));

    let over_floor = RankingProfile {
        thresholds: DecisionThresholds {
            none_below: Score::from_millionths(top.millionths() + 1),
            direct_at_least: Score::from_millionths(top.millionths() + 1),
            ..exactly.thresholds
        },
        ..DEFAULT_RANKING_PROFILE
    };
    assert!(
        matches!(
            decide(ranked, &over_floor).unwrap(),
            QueryResult::None {
                reason: NoneReason::LowConfidence,
                ..
            }
        ),
        "the abstention floor is exclusive, so only a score strictly below it abstains"
    );
}

#[test]
fn ranking_scoring_ignores_the_order_files_are_indexed() {
    const SOURCES: [(&str, &str); 4] = [
        ("src/a.rs", "/// Reads a packet.\npub fn read_packet() {}\n"),
        (
            "src/b.rs",
            "/// Writes a packet.\npub fn write_packet() {}\n",
        ),
        ("src/c.rs", "/// Drops a packet.\npub fn drop_packet() {}\n"),
        (
            "src/d.rs",
            "/// Counts a packet.\npub fn count_packet() {}\n",
        ),
    ];
    let sorted = synthetic_snapshot(&SOURCES);
    let mut shuffled = SOURCES;
    shuffled.reverse();
    let reversed = synthetic_snapshot(&shuffled);

    let mut scratch = RankingScratch::new();
    let ordering = |snapshot: &Snapshot, scratch: &mut RankingScratch| {
        let parsed = parse_query(snapshot, QueryRequest::new("packet", None)).unwrap();
        let (ranked, evidence) =
            rank(snapshot, &parsed.terms, &DEFAULT_RANKING_PROFILE, scratch).unwrap();
        let anchors: Vec<(String, Score)> = ranked
            .iter()
            .map(|entry| (support::anchor_of(snapshot, entry.symbol), entry.score))
            .collect();
        (anchors, evidence.posting_hits_scanned)
    };

    assert_eq!(
        ordering(&sorted, &mut scratch),
        ordering(&reversed, &mut scratch),
        "the builder sorts its inputs, so the order they arrive in cannot reach a score"
    );
}

#[test]
fn ranking_scoring_repeats_byte_for_byte() {
    let snapshot = fixture_snapshot("wide");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how is an event handled?", None),
    )
    .unwrap();
    let first: Vec<(usize, Score)> = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap()
    .0
    .iter()
    .map(|entry| (entry.symbol.index(), entry.score))
    .collect();

    for run in 0..100 {
        let (ranked, _evidence) = rank(
            &snapshot,
            &parsed.terms,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .unwrap();
        let again: Vec<(usize, Score)> = ranked
            .iter()
            .map(|entry| (entry.symbol.index(), entry.score))
            .collect();
        assert_eq!(first, again, "run {run} ranked differently");
    }
}

#[test]
fn ranking_scoring_reports_corrupt_corpus_statistics() {
    let mut snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how do we create a session?", None),
    )
    .unwrap();
    snapshot.corpus.fields[LexicalField::Name.index()].total_term_frequency = 0;

    let error = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap_err();
    assert!(
        matches!(error, RankingError::InvalidCorpusStats { .. }),
        "a corrupt index must be reported, never scored into a confident wrong answer: {error:?}"
    );
}

#[test]
fn ranking_scoring_never_ranks_an_exact_ambiguity() {
    let snapshot = synthetic_snapshot(&[
        ("src/left.rs", "/// Left.\npub fn shared_name() {}\n"),
        ("src/right.rs", "/// Right.\npub fn shared_name() {}\n"),
    ]);
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(&snapshot, QueryRequest::new("shared_name", None)).unwrap();

    let result = route_query(&snapshot, &parsed, &DEFAULT_RANKING_PROFILE, &mut scratch).unwrap();
    let pipeline = match &result {
        QueryResult::Direct { pipeline, .. }
        | QueryResult::Candidates { pipeline, .. }
        | QueryResult::None { pipeline, .. } => *pipeline,
    };
    assert_eq!(
        pipeline,
        Pipeline::Exact,
        "an exact name that resolves to more than one definition is answered by the exact \
         router; handing it to the ranker would rank an answer the router already has: \
         {result:?}"
    );
}

#[test]
fn ranking_scoring_abstains_on_a_repository_with_nothing_to_rank() {
    let snapshot = synthetic_snapshot(&[("src/empty.rs", "// nothing is defined here\n")]);
    assert!(snapshot.symbols.is_empty(), "the corpus defines no symbol");

    let mut scratch = RankingScratch::new();
    let parsed = parse_query(&snapshot, QueryRequest::new("anything at all", None)).unwrap();
    let (ranked, evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();

    assert!(ranked.is_empty());
    assert_eq!(evidence.effective_query_terms, 0);
    assert_eq!(evidence.posting_hits_scanned, 0);
    assert_eq!(
        decide(ranked, &DEFAULT_RANKING_PROFILE).unwrap(),
        QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Lexical,
        },
        "an index with nothing in it must abstain rather than fail"
    );
}

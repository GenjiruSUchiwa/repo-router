//! Behaviour of the lexical fallback against the fixture repositories.

#![allow(clippy::unwrap_used)]

mod support;

use rr_core::index::SnapshotMeta;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, route_query, QueryRequest};
use rr_core::ranking::{
    rank, MarginPpm, RankingError, RankingScratch, Score, CANDIDATE_LIMIT, DEFAULT_RANKING_PROFILE,
    RESULT_LIMIT,
};
use rr_core::render::render_json;
use rr_core::result::{NoneReason, Pipeline, QueryResult};
use support::{anchor_of, fixture_snapshot, lexical, result_anchors, symbol_named};

#[test]
fn ranking_answers_a_named_definition_directly() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let (result, evidence) = lexical(&snapshot, "how do we create a session?", &mut scratch);

    let QueryResult::Direct {
        candidate,
        pipeline,
        ..
    } = &result
    else {
        panic!("expected a direct answer, got {result:?}");
    };
    assert_eq!(*pipeline, Pipeline::Lexical);
    assert_eq!(
        result_anchors(&snapshot, &result),
        ["src/auth/session.rs#create_session"]
    );
    assert_eq!(evidence.effective_query_terms, 2);
    let confidence = candidate.confidence.expect("a direct answer is confident");
    assert!(
        confidence.get() >= 0.2,
        "a direct answer reports its normalized margin, got {}",
        confidence.get()
    );
}

#[test]
fn ranking_offers_candidates_when_two_definitions_tie() {
    let snapshot = fixture_snapshot("storage");
    let mut scratch = RankingScratch::new();
    let (result, _evidence) = lexical(&snapshot, "wire representation of an entry", &mut scratch);

    let QueryResult::Candidates {
        candidates,
        pipeline,
    } = &result
    else {
        panic!("expected candidates, got {result:?}");
    };
    assert_eq!(*pipeline, Pipeline::Lexical);
    assert_eq!(candidates.len(), RESULT_LIMIT);
    assert!(
        candidates.iter().all(|entry| entry.confidence.is_none()),
        "a candidate list carries no confidence"
    );
    assert_eq!(
        result_anchors(&snapshot, &result)[..2],
        [
            "src/store/serialize.rs#serialize_entry",
            "src/store/serialize.rs#deserialize_entry"
        ]
    );
}

#[test]
fn ranking_abstains_when_every_word_is_unknown() {
    let snapshot = fixture_snapshot("geometry");
    let mut scratch = RankingScratch::new();
    let (result, evidence) = lexical(&snapshot, "security logic", &mut scratch);

    assert_eq!(
        result,
        QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Lexical,
        }
    );
    assert_eq!(evidence.effective_query_terms, 0);
    assert_eq!(evidence.posting_hits_scanned, 0);
}

#[test]
fn ranking_abstains_when_the_evidence_is_incidental() {
    let snapshot = fixture_snapshot("wide");
    let mut scratch = RankingScratch::new();
    let (result, evidence) = lexical(&snapshot, "which kind is escalated?", &mut scratch);

    assert_eq!(
        result,
        QueryResult::None {
            reason: NoneReason::LowConfidence,
            pipeline: Pipeline::Lexical,
        }
    );
    assert_eq!(evidence.effective_query_terms, 1);
}

#[test]
fn ranking_orders_candidates_by_score_then_symbol_id() {
    let snapshot = fixture_snapshot("wide");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how is an event handled?", None),
    )
    .unwrap();
    let (ranked, _evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();

    assert!(ranked.windows(2).all(|pair| {
        pair[0].score > pair[1].score
            || (pair[0].score == pair[1].score && pair[0].symbol < pair[1].symbol)
    }));
    let handlers: Vec<String> = ranked
        .iter()
        .take(3)
        .map(|entry| anchor_of(&snapshot, entry.symbol))
        .collect();
    assert_eq!(
        handlers,
        [
            "src/wide/handlers.rs#handle_event_01",
            "src/wide/handlers.rs#handle_event_02",
            "src/wide/handlers.rs#handle_event_03"
        ],
        "equal scores fall back to source order"
    );
}

#[test]
fn ranking_caps_the_candidate_union_at_the_profile_limit() {
    let snapshot = fixture_snapshot("wide");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how is an event handled?", None),
    )
    .unwrap();
    let (ranked, evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();

    assert!(
        evidence.candidates_before_cap > CANDIDATE_LIMIT as u64,
        "the fixture must overflow the candidate cap"
    );
    assert_eq!(ranked.len(), CANDIDATE_LIMIT);
    assert_eq!(evidence.candidates_scored as usize, CANDIDATE_LIMIT);
    assert_eq!(
        evidence.candidates_dropped,
        evidence.candidates_before_cap - CANDIDATE_LIMIT as u64
    );
    assert!(
        ranked
            .iter()
            .any(|entry| entry.symbol == symbol_named(&snapshot, "handle_event_01")),
        "the cap keeps the rare term's postings, not the first ones merged"
    );
}

#[test]
fn ranking_ignores_the_order_of_the_query_words() {
    let snapshot = fixture_snapshot("http");
    let mut scratch = RankingScratch::new();
    let (straight, _) = lexical(
        &snapshot,
        "retry a failed request with exponential backoff",
        &mut scratch,
    );
    let (shuffled, _) = lexical(
        &snapshot,
        "backoff exponential with request failed a retry",
        &mut scratch,
    );

    assert_eq!(straight, shuffled);
}

#[test]
fn ranking_treats_a_repeated_word_as_one_term() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("where is token verification handled?", None),
    )
    .unwrap();
    let (ranked, evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    let once: Vec<(Score, String)> = ranked
        .iter()
        .map(|entry| (entry.score, anchor_of(&snapshot, entry.symbol)))
        .collect();
    let scanned = evidence.posting_hits_scanned;

    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("token token verification verification", None),
    )
    .unwrap();
    let (ranked, evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    let twice: Vec<(Score, String)> = ranked
        .iter()
        .map(|entry| (entry.score, anchor_of(&snapshot, entry.symbol)))
        .collect();

    assert_eq!(once, twice);
    assert_eq!(scanned, evidence.posting_hits_scanned);
}

#[test]
fn ranking_runs_only_after_the_exact_router_misses() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();

    let parsed = parse_query(&snapshot, QueryRequest::new("verify_token", None)).unwrap();
    let (exact, _) =
        route_query(&snapshot, &parsed, &DEFAULT_RANKING_PROFILE, &mut scratch).unwrap();
    assert!(matches!(
        exact,
        QueryResult::Direct {
            pipeline: Pipeline::Exact,
            ..
        }
    ));

    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how do we create a session?", None),
    )
    .unwrap();
    let (fallback, _) =
        route_query(&snapshot, &parsed, &DEFAULT_RANKING_PROFILE, &mut scratch).unwrap();
    assert!(matches!(
        fallback,
        QueryResult::Direct {
            pipeline: Pipeline::Lexical,
            ..
        }
    ));
}

#[test]
fn ranking_is_suppressed_by_a_path_qualifier() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let path = RelPath::new("src/auth/session.rs").unwrap();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how do we create a session?", Some(&path)),
    )
    .unwrap();

    let (result, _) =
        route_query(&snapshot, &parsed, &DEFAULT_RANKING_PROFILE, &mut scratch).unwrap();
    assert_eq!(
        result,
        QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Exact,
        },
        "a path qualifier the exact router cannot satisfy abstains instead of \
         answering from another file"
    );
}

#[test]
fn ranking_reuses_one_scratch_across_repositories() {
    let auth = fixture_snapshot("auth");
    let wide = fixture_snapshot("wide");
    let mut shared = RankingScratch::new();
    let mut fresh = RankingScratch::new();

    let (wide_first, _) = lexical(&wide, "how is an event handled?", &mut shared);
    let (auth_shared, _) = lexical(&auth, "how do we create a session?", &mut shared);
    let (auth_fresh, _) = lexical(&auth, "how do we create a session?", &mut fresh);
    let (wide_again, _) = lexical(&wide, "how is an event handled?", &mut shared);

    assert_eq!(auth_shared, auth_fresh);
    assert_eq!(wide_first, wide_again);
}

#[test]
fn ranking_rejects_a_profile_the_snapshot_does_not_stamp() {
    let snapshot = fixture_snapshot("auth");
    let mut scratch = RankingScratch::new();
    let mut profile = DEFAULT_RANKING_PROFILE;
    profile.version += 1;
    let parsed = parse_query(&snapshot, QueryRequest::new("session", None)).unwrap();

    let error = rank(&snapshot, &parsed.terms, &profile, &mut scratch).unwrap_err();
    assert!(matches!(error, RankingError::ProfileMismatch { .. }));
}

#[test]
fn ranking_rejects_a_profile_whose_direct_floor_is_below_the_abstention_floor() {
    let mut profile = DEFAULT_RANKING_PROFILE;
    profile.thresholds.direct_at_least = Score::from_millionths(1);
    profile.thresholds.none_below = Score::from_millionths(2);
    assert!(matches!(
        profile.validate().unwrap_err(),
        RankingError::InvalidProfile { .. }
    ));
}

#[test]
fn ranking_stamps_every_snapshot_with_the_shipped_profile() {
    let snapshot = fixture_snapshot("auth");
    assert!(snapshot.meta.ranking.accepts(&DEFAULT_RANKING_PROFILE));
    assert_eq!(
        snapshot.meta.ranking,
        SnapshotMeta::new(None, true, [0; 32]).ranking,
        "every builder stamps the same profile"
    );
}

#[test]
fn ranking_renders_a_direct_answer_as_the_public_json_contract() {
    let snapshot = fixture_snapshot("http");
    let mut scratch = RankingScratch::new();
    let (result, _evidence) = lexical(
        &snapshot,
        "how do we apply middleware to a request?",
        &mut scratch,
    );
    let rendered = render_json(&snapshot, &result).unwrap();
    let value: serde_json::Value = serde_json::from_str(rendered.trim_end()).unwrap();

    assert_eq!(value["v"], 1);
    assert_eq!(value["result"], "direct");
    assert_eq!(value["pipeline"], "lexical");
    assert_eq!(value["anchor"]["path"], "src/http/middleware.rs");
    assert_eq!(value["anchor"]["symbol"], "apply_middleware");
    let confidence = value["confidence"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&confidence));
}

#[test]
fn ranking_reports_a_confidence_that_is_the_normalized_margin() {
    let snapshot = fixture_snapshot("http");
    let mut scratch = RankingScratch::new();
    let parsed = parse_query(
        &snapshot,
        QueryRequest::new("how do we apply middleware to a request?", None),
    )
    .unwrap();
    let (ranked, _evidence) = rank(
        &snapshot,
        &parsed.terms,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .unwrap();
    let margin = MarginPpm::between(ranked[0].score, ranked[1].score);

    let (result, _evidence) = lexical(
        &snapshot,
        "how do we apply middleware to a request?",
        &mut scratch,
    );
    let QueryResult::Direct { candidate, .. } = result else {
        panic!("expected a direct answer");
    };
    let confidence = candidate.confidence.unwrap();
    assert!((f64::from(confidence.get()) - f64::from(margin.get()) / 1_000_000.0).abs() < 1e-6);
}

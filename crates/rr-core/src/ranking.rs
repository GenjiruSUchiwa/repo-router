//! Deterministic per-field BM25 ranking and corpus-calibrated abstention.
//!
//! This is the lexical fallback invoked by [`crate::query::route_query`] only
//! when [`crate::query::route_exact`] returns
//! [`ExactOutcome::Miss`](crate::query::ExactOutcome::Miss). Exact routing
//! always wins: no score computed here can displace a unique exact result, and
//! there is no exact-match bonus inside the formula.
//!
//! One searchable document is one indexed symbol, and its content is the
//! field-term multiset produced by [`crate::lex`] — never raw source text.
//! Query term frequency is binary: [`QueryTerms`] is already first-occurrence
//! ordered and globally deduplicated, so repeating a word cannot amplify it.
//! Nothing here lowercases, splits, stems, or drops stop words a second time.
//!
//! The scoring model is additive **per-field BM25**: every field carries its own
//! inverse document frequency, length normalization, saturation, and boost, and
//! the contributions are summed after saturation. Field-local statistics follow
//! Lucene's sparse-field rationale, where `docFreq` and `docCount` describe the
//! same population.
//!
//! Ranking and decisions consume only quantized [`Score`] values, so results,
//! ties, and diagnostics never depend on raw floating point.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fmt;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::index::{PostingList, Snapshot, SymbolId, SymbolPosting};
use crate::lex::{LexicalField, QueryTerms, TermId};
use crate::result::{Candidate, Confidence, NoneReason, Pipeline, QueryResult, TargetId};

/// Number of union members retained for scoring by the default profile.
pub const CANDIDATE_LIMIT: usize = 64;
/// Maximum number of entries in a [`QueryResult::Candidates`] answer.
pub const RESULT_LIMIT: usize = 3;
/// Fixed-point denominator of [`Score`]: one score unit is a million millionths.
pub const SCORE_SCALE: u64 = 1_000_000;
/// Fixed-point denominator of [`MarginPpm`]: parts per million.
pub const MARGIN_SCALE: u32 = 1_000_000;

/// Semantic version of [`DEFAULT_RANKING_PROFILE`]; bumped by any profile change.
pub const RANKING_PROFILE_VERSION: u32 = 1;

/// [`SCORE_SCALE`] as the float the quantizer multiplies by.
const SCORE_SCALE_F64: f64 = 1_000_000.0;
/// [`MARGIN_SCALE`] as the float the confidence conversion divides by.
const MARGIN_SCALE_F64: f64 = 1_000_000.0;
/// Largest integer that `f64` represents exactly (2^53); the quantization bound.
const MAX_EXACT_INTEGER_F64: f64 = 9_007_199_254_740_992.0;
/// Sentinel for "this (query term, field) pair has no posting list".
const NO_STREAM: u32 = u32::MAX;

/// A quantized BM25 score in millionths of one score unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Score(u64);

impl Score {
    /// The zero score, used for a missing runner-up.
    pub const ZERO: Self = Self(0);

    /// Builds a score from an exact count of millionths.
    #[must_use]
    pub const fn from_millionths(millionths: u64) -> Self {
        Self(millionths)
    }

    /// Returns the exact count of millionths backing this score.
    #[must_use]
    pub const fn millionths(self) -> u64 {
        self.0
    }

    /// Quantizes an accumulated raw score with round-half-up.
    ///
    /// Overflowing or non-finite input is rejected rather than clamped: a
    /// silently saturated score would corrupt every downstream comparison.
    fn quantize(raw: f64) -> Result<Self, RankingError> {
        if !raw.is_finite() || raw < 0.0 {
            return Err(RankingError::Arithmetic {
                reason: "raw score is not finite and non-negative",
            });
        }
        let scaled = raw * SCORE_SCALE_F64 + 0.5;
        if !scaled.is_finite() || scaled >= MAX_EXACT_INTEGER_F64 {
            return Err(RankingError::Arithmetic {
                reason: "quantized score exceeds the exact integer range",
            });
        }
        Ok(Self(truncate_to_u64(scaled.floor())))
    }
}

impl fmt::Display for Score {
    /// Formats the score with exactly six fractional digits, never scientific
    /// notation, so goldens and diagnostics are byte-stable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:06}", self.0 / SCORE_SCALE, self.0 % SCORE_SCALE)
    }
}

impl Serialize for Score {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A normalized top-two margin in parts per million, inclusive `0..=1_000_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize)]
#[serde(transparent)]
pub struct MarginPpm(u32);

impl MarginPpm {
    /// A zero margin: the top two scores are equal, or the top score is zero.
    pub const ZERO: Self = Self(0);
    /// The maximum margin, reached when there is no runner-up.
    pub const FULL: Self = Self(MARGIN_SCALE);

    /// Builds a margin from parts per million.
    ///
    /// # Errors
    /// Returns [`RankingError::InvalidProfile`] if `ppm` exceeds [`MARGIN_SCALE`].
    pub const fn new(ppm: u32) -> Result<Self, RankingError> {
        if ppm > MARGIN_SCALE {
            return Err(RankingError::InvalidProfile {
                reason: "margin exceeds one million parts per million",
            });
        }
        Ok(Self(ppm))
    }

    /// Returns the margin in parts per million.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Computes `(top - runner_up) / top` in parts per million.
    ///
    /// `u128` multiplication keeps the ratio exact for every representable
    /// score pair. A zero top score has, by definition, a zero margin.
    #[must_use]
    pub fn between(top: Score, runner_up: Score) -> Self {
        if top.0 == 0 {
            return Self::ZERO;
        }
        let difference = u128::from(top.0.saturating_sub(runner_up.0));
        let scaled = difference * u128::from(MARGIN_SCALE) / u128::from(top.0);
        Self(u32::try_from(scaled).unwrap_or(MARGIN_SCALE))
    }

    /// Converts the margin into the public confidence value of a direct answer.
    ///
    /// This is a calibrated normalized margin, not a probability.
    fn into_confidence(self) -> Result<Confidence, RankingError> {
        Confidence::new(narrow_to_f32(f64::from(self.0) / MARGIN_SCALE_F64)).map_err(|_| {
            RankingError::Arithmetic {
                reason: "margin does not convert into a valid confidence",
            }
        })
    }
}

/// BM25 parameters of one lexical field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldParams {
    /// Multiplier applied to this field's saturated contribution.
    pub boost: f64,
    /// Term-frequency saturation parameter.
    pub k1: f64,
    /// Length-normalization strength, `0.0..=1.0`.
    pub b: f64,
}

impl FieldParams {
    /// A field that contributes nothing, used to seed a profile table.
    pub const UNSCORED: Self = Self {
        boost: 0.0,
        k1: 0.0,
        b: 0.0,
    };

    /// Builds the parameters of one field.
    #[must_use]
    pub const fn new(boost: f64, k1: f64, b: f64) -> Self {
        Self { boost, k1, b }
    }
}

/// Frozen statistics of one lexical field over the whole indexed corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FieldStats {
    /// Number of symbols whose field length is greater than zero.
    pub document_count: u32,
    /// Sum of every symbol's field length.
    pub total_term_frequency: u64,
}

/// Per-field corpus statistics, computed once while the index is frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusStats {
    /// One entry per lexical field, in canonical declaration order.
    pub fields: [FieldStats; LexicalField::COUNT],
}

impl CorpusStats {
    /// All-zero statistics, matching an index with no searchable symbol.
    pub const EMPTY: Self = Self {
        fields: [FieldStats {
            document_count: 0,
            total_term_frequency: 0,
        }; LexicalField::COUNT],
    };

    /// Returns the frozen statistics of one field.
    #[must_use]
    pub const fn field(&self, field: LexicalField) -> FieldStats {
        self.fields[field.index()]
    }

    /// Returns `total_term_frequency / document_count`, or `None` for an empty field.
    fn average_length(&self, field: LexicalField) -> Result<Option<f64>, RankingError> {
        let stats = self.field(field);
        if stats.document_count == 0 {
            if stats.total_term_frequency != 0 {
                return Err(RankingError::InvalidCorpusStats {
                    reason: "empty field carries a non-zero total term frequency",
                });
            }
            return Ok(None);
        }
        if stats.total_term_frequency < u64::from(stats.document_count) {
            return Err(RankingError::InvalidCorpusStats {
                reason: "total term frequency is below the field document count",
            });
        }
        let total = widen_to_f64(stats.total_term_frequency)?;
        let average = total / f64::from(stats.document_count);
        if !average.is_finite() || average <= 0.0 {
            return Err(RankingError::InvalidCorpusStats {
                reason: "average field length is not finite and positive",
            });
        }
        Ok(Some(average))
    }
}

/// Calibrated thresholds separating `direct`, `candidates`, and `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DecisionThresholds {
    /// Exclusive floor: a top score strictly below this abstains.
    pub none_below: Score,
    /// Inclusive absolute floor a direct answer must clear.
    pub direct_at_least: Score,
    /// Inclusive normalized top-two margin a direct answer must clear.
    pub direct_margin_at_least: MarginPpm,
}

/// A validated, versioned ranking configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingProfile {
    /// Semantic version of this profile.
    pub version: u32,
    /// Per-field parameters, in canonical declaration order.
    pub fields: [FieldParams; LexicalField::COUNT],
    /// Calibrated decision thresholds.
    pub thresholds: DecisionThresholds,
    /// Maximum number of union members retained for scoring.
    pub candidate_limit: u16,
    /// Maximum number of entries in a candidate answer.
    pub result_limit: u8,
}

/// The initial profile.
///
/// The boosts encode the product preference for names, qualifications, paths,
/// and signatures over noisy bodies; they are explicit inputs, not claims from
/// the BM25 literature. Each row is written through its own field ordinal, so
/// the table cannot silently drift out of declaration order.
///
/// The thresholds are not hand-picked: they are copied verbatim from
/// `tests/fixtures/ranking/calibration.expected.json`, which the
/// `ranking_calibration` suite recomputes and compares byte for byte.
pub const DEFAULT_RANKING_PROFILE: RankingProfile = RankingProfile {
    version: RANKING_PROFILE_VERSION,
    fields: {
        let mut fields = [FieldParams::UNSCORED; LexicalField::COUNT];
        fields[LexicalField::Name.index()] = FieldParams::new(8.0, 1.2, 0.00);
        fields[LexicalField::Qualified.index()] = FieldParams::new(5.0, 1.2, 0.30);
        fields[LexicalField::Path.index()] = FieldParams::new(5.0, 1.2, 0.30);
        fields[LexicalField::Signature.index()] = FieldParams::new(4.0, 1.2, 0.75);
        fields[LexicalField::Body.index()] = FieldParams::new(1.5, 1.2, 0.75);
        fields[LexicalField::Documentation.index()] = FieldParams::new(2.0, 1.2, 0.75);
        fields[LexicalField::Attribute.index()] = FieldParams::new(1.0, 1.2, 0.50);
        fields[LexicalField::Callee.index()] = FieldParams::new(3.0, 1.2, 0.50);
        fields[LexicalField::Caller.index()] = FieldParams::new(3.0, 1.2, 0.50);
        fields[LexicalField::Import.index()] = FieldParams::new(2.0, 1.2, 0.50);
        fields
    },
    thresholds: DecisionThresholds {
        none_below: Score::from_millionths(20_770_970),
        direct_at_least: Score::from_millionths(30_723_125),
        direct_margin_at_least: MarginPpm(181_211),
    },
    candidate_limit: 64,
    result_limit: 3,
};

impl RankingProfile {
    /// Returns the parameters of one field.
    #[must_use]
    pub const fn field(&self, field: LexicalField) -> FieldParams {
        self.fields[field.index()]
    }

    /// Checks every load-time invariant of the profile.
    ///
    /// The result limit is bounded by [`RESULT_LIMIT`] because the public v1
    /// result contract carries at most that many candidates: a larger limit
    /// would build a result no caller can render.
    ///
    /// # Errors
    /// Returns [`RankingError::InvalidProfile`] describing the first violation.
    pub fn validate(&self) -> Result<(), RankingError> {
        for params in &self.fields {
            if !params.boost.is_finite() || params.boost < 0.0 {
                return Err(RankingError::InvalidProfile {
                    reason: "field boost must be finite and non-negative",
                });
            }
            if !params.k1.is_finite() || params.k1 < 0.0 {
                return Err(RankingError::InvalidProfile {
                    reason: "field k1 must be finite and non-negative",
                });
            }
            if !params.b.is_finite() || !(0.0..=1.0).contains(&params.b) {
                return Err(RankingError::InvalidProfile {
                    reason: "field b must be finite and within 0.0..=1.0",
                });
            }
        }
        if self.thresholds.direct_at_least < self.thresholds.none_below {
            return Err(RankingError::InvalidProfile {
                reason: "a direct answer must clear the abstention floor",
            });
        }
        if self.candidate_limit == 0 || self.result_limit == 0 {
            return Err(RankingError::InvalidProfile {
                reason: "candidate and result limits must be non-zero",
            });
        }
        if usize::from(self.candidate_limit) < usize::from(self.result_limit) {
            return Err(RankingError::InvalidProfile {
                reason: "candidate limit must be at least the result limit",
            });
        }
        if usize::from(self.result_limit) > RESULT_LIMIT {
            return Err(RankingError::InvalidProfile {
                reason: "result limit exceeds the public candidate contract",
            });
        }
        Ok(())
    }

    /// Digest of the parameters that determine a score.
    ///
    /// Thresholds are deliberately excluded: they are decision-time policy and
    /// leave every score unchanged, which is what lets calibration explore them
    /// against one frozen snapshot. Any threshold change still bumps
    /// [`RankingProfile::version`], which is stamped alongside this digest.
    #[must_use]
    pub fn scoring_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.version.to_le_bytes());
        for params in &self.fields {
            hasher.update(&params.boost.to_bits().to_le_bytes());
            hasher.update(&params.k1.to_bits().to_le_bytes());
            hasher.update(&params.b.to_bits().to_le_bytes());
        }
        hasher.update(&self.candidate_limit.to_le_bytes());
        hasher.update(&self.result_limit.to_le_bytes());
        *hasher.finalize().as_bytes()
    }
}

/// The ranking configuration an index was frozen under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankingStamp {
    /// [`RankingProfile::version`] of the profile in force at build time.
    pub profile_version: u32,
    /// [`RankingProfile::scoring_digest`] of that profile.
    pub scoring_digest: [u8; 32],
}

impl RankingStamp {
    /// Stamps a profile into a form storable in a snapshot header.
    #[must_use]
    pub fn of(profile: &RankingProfile) -> Self {
        Self {
            profile_version: profile.version,
            scoring_digest: profile.scoring_digest(),
        }
    }

    /// Reports whether a profile may score against a snapshot carrying this stamp.
    #[must_use]
    pub fn accepts(&self, profile: &RankingProfile) -> bool {
        self.profile_version == profile.version && self.scoring_digest == profile.scoring_digest()
    }
}

/// Why a lexical route could not be computed.
///
/// Every variant is an execution error, never a silent `none`: index corruption
/// must not be reported to an agent as "no match".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RankingError {
    #[error("invalid ranking profile: {reason}")]
    InvalidProfile { reason: &'static str },
    #[error("ranking profile v{profile} does not match the indexed profile v{snapshot}")]
    ProfileMismatch { snapshot: u32, profile: u32 },
    #[error("invalid corpus statistics: {reason}")]
    InvalidCorpusStats { reason: &'static str },
    #[error("corrupt postings: {reason}")]
    InvalidPostings { reason: &'static str },
    #[error("invalid query terms: {reason}")]
    InvalidQuery { reason: &'static str },
    #[error("query term {term} is outside the snapshot lexicon of {terms} terms")]
    TermOutOfRange { term: usize, terms: usize },
    #[error("symbol {symbol} is outside the snapshot of {symbols} symbols")]
    SymbolOutOfRange { symbol: usize, symbols: usize },
    #[error("query carries {terms} distinct terms, more than the ranker admits")]
    QueryTooLarge { terms: usize },
    #[error("ranking arithmetic failed: {reason}")]
    Arithmetic { reason: &'static str },
}

/// Bounded work counters for one lexical route; diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct RankingEvidence {
    /// Posting entries consumed by the multiway merge.
    pub posting_hits_scanned: u64,
    /// Distinct symbols in the posting union, counted before the cap.
    pub candidates_before_cap: u64,
    /// Symbols actually scored.
    pub candidates_scored: u16,
    /// Union members the cap discarded; truncation is never silent.
    pub candidates_dropped: u64,
    /// Whether the cap fell inside a group of members it could not tell apart.
    ///
    /// The retention key ranks on `(rarest_df, matched_terms)` and breaks the
    /// remaining ties on `SymbolId`, which carries no evidence at all — it is
    /// the order the files happened to be indexed in. When the worst retained
    /// member ranks equal to a discarded one, the cap chose between them on
    /// that arbitrary tie-break, so a discarded member could have outscored
    /// every candidate that survived.
    ///
    /// This is the counter to read, not [`RankingEvidence::candidates_dropped`]:
    /// discarding thousands of clearly weaker members is the design working,
    /// while cutting through a tie means the answer is one of several the
    /// ranker had no basis to choose between.
    pub cap_cut_a_tie: bool,
    /// Query terms that opened at least one posting stream.
    pub effective_query_terms: u16,
}

/// One scored symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RankedSymbol {
    /// The scored symbol.
    pub symbol: SymbolId,
    /// Its quantized score.
    pub score: Score,
}

/// One `(query term, field)` posting cursor.
#[derive(Debug, Clone, Copy)]
struct Stream {
    field: LexicalField,
    /// Index of the query term in the ascending `TermId` order.
    term_slot: u16,
    /// Index of the posting list inside the field's list vector.
    list: u32,
    /// Next unread entry.
    cursor: u32,
    /// Number of entries, which is exactly this `(term, field)` document frequency.
    length: u32,
    /// Field-local inverse document frequency of the term.
    idf: f64,
}

/// A stream's current head, ordered for the merge min-heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StreamHead {
    symbol: SymbolId,
    /// Ties break on the stream index, keeping the heap order total.
    stream: u32,
}

/// A union member competing for one of the retained slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Retained {
    rarest_df: u32,
    matched_terms: u16,
    symbol: SymbolId,
}

impl Retained {
    /// The part of the retention key that carries evidence.
    ///
    /// `SymbolId` is excluded: it only makes the order total, and two members
    /// sharing this pair are ones the cap has no evidence to choose between.
    const fn signal(self) -> (u32, u16) {
        (self.rarest_df, self.matched_terms)
    }
}

impl Ord for Retained {
    /// Orders **best first**: this is the retention key
    /// `(rarest_df ascending, matched_terms descending, SymbolId ascending)`
    /// exactly, so the most promising member is the `Ordering::Less` one, and
    /// the order is total because symbol ids are unique.
    ///
    /// [`retain`] therefore holds the members in a **max**-heap, whose root is
    /// the worst member kept and the only one a new arrival may displace.
    /// Reversing this comparison would silently make the cap keep the least
    /// promising union members instead of the best ones.
    fn cmp(&self, other: &Self) -> Ordering {
        self.rarest_df
            .cmp(&other.rarest_df)
            .then_with(|| other.matched_terms.cmp(&self.matched_terms))
            .then_with(|| self.symbol.cmp(&other.symbol))
    }
}

impl PartialOrd for Retained {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Reusable bounded buffers for [`rank`].
///
/// Capacity is `O(query terms × fields + candidate limit)` and never depends on
/// the number of indexed symbols. After the first call of a given shape, a
/// second identical call performs no allocation at all.
#[derive(Debug, Default)]
pub struct RankingScratch {
    /// Query terms in ascending `TermId` order; the position is the term slot.
    terms: Vec<TermId>,
    streams: Vec<Stream>,
    /// `slot × LexicalField::COUNT + field` to stream index, or [`NO_STREAM`].
    stream_of: Vec<u32>,
    merge: BinaryHeap<Reverse<StreamHead>>,
    /// Streams whose head is the symbol currently being merged.
    group: Vec<u32>,
    /// Per-slot "already counted for this symbol" flags.
    seen: Vec<bool>,
    retained: BinaryHeap<Retained>,
    ranked: Vec<RankedSymbol>,
}

impl RankingScratch {
    /// Creates empty scratch space.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserves every buffer for a query of `terms` terms under `profile`.
    ///
    /// Reserving up front is what makes the merge allocation-free: no buffer can
    /// grow while posting hits are being consumed.
    fn reserve(&mut self, terms: usize, profile: &RankingProfile) {
        let streams = terms.saturating_mul(LexicalField::COUNT);
        let cap = usize::from(profile.candidate_limit);
        clear_and_reserve(&mut self.terms, terms);
        clear_and_reserve(&mut self.streams, streams);
        clear_and_reserve(&mut self.stream_of, streams);
        clear_and_reserve(&mut self.group, streams);
        clear_and_reserve(&mut self.seen, terms);
        clear_and_reserve(&mut self.ranked, cap);
        self.merge.clear();
        self.merge.reserve(streams);
        self.retained.clear();
        self.retained.reserve(cap);
    }
}

fn clear_and_reserve<T>(buffer: &mut Vec<T>, capacity: usize) {
    buffer.clear();
    buffer.reserve(capacity);
}

/// Generates, scores, and orders the lexical candidates of one query.
///
/// The returned slice borrows `scratch` and is ordered by `(Score descending,
/// SymbolId ascending)`. It is empty when the query has no terms, no posting
/// hits, or no scored candidate.
///
/// # Errors
/// Returns [`RankingError`] for an invalid profile, a profile that does not
/// match the snapshot, inconsistent statistics, corrupt postings, out-of-range
/// ids, or non-finite arithmetic.
pub fn rank<'scratch>(
    snapshot: &Snapshot,
    query: &QueryTerms,
    profile: &RankingProfile,
    scratch: &'scratch mut RankingScratch,
) -> Result<(&'scratch [RankedSymbol], RankingEvidence), RankingError> {
    profile.validate()?;
    if !snapshot.meta.ranking.accepts(profile) {
        return Err(RankingError::ProfileMismatch {
            snapshot: snapshot.meta.ranking.profile_version,
            profile: profile.version,
        });
    }
    scratch.reserve(query.len(), profile);
    load_query_terms(query, scratch)?;
    let effective_query_terms = open_streams(snapshot, scratch)?;
    let mut evidence = RankingEvidence {
        effective_query_terms,
        ..RankingEvidence::default()
    };
    merge_candidates(snapshot, profile, scratch, &mut evidence)?;
    score_retained(snapshot, profile, scratch, &mut evidence)?;
    Ok((scratch.ranked.as_slice(), evidence))
}

/// Turns ranked candidates into the public result vocabulary.
///
/// The decision order is exact: an empty ranking abstains, a top score below
/// [`DecisionThresholds::none_below`] abstains, a top score clearing both direct
/// thresholds answers directly, and anything else returns candidates.
///
/// Only a direct answer carries a confidence. A candidate list reports evidence
/// the ranker judged too weak to answer with, and attaching an uncalibrated
/// pseudo-probability to it would misrepresent that.
///
/// # Errors
/// Returns [`RankingError::InvalidProfile`] if `profile` violates a load-time
/// invariant — notably a `result_limit` above [`RESULT_LIMIT`], which would
/// build a candidate list the public v1 contract cannot carry — or
/// [`RankingError::Arithmetic`] if the margin cannot be represented as a public
/// confidence.
pub fn decide(
    ranked: &[RankedSymbol],
    profile: &RankingProfile,
) -> Result<QueryResult, RankingError> {
    profile.validate()?;
    let thresholds = profile.thresholds;
    let Some(top) = ranked.first() else {
        return Ok(none(NoneReason::NotFound));
    };
    if top.score < thresholds.none_below {
        return Ok(none(NoneReason::LowConfidence));
    }

    let runner_up = ranked.get(1).map_or(Score::ZERO, |entry| entry.score);
    let margin = MarginPpm::between(top.score, runner_up);
    if top.score >= thresholds.direct_at_least && margin >= thresholds.direct_margin_at_least {
        return Ok(QueryResult::Direct {
            candidate: Candidate::new(
                TargetId::Symbol(top.symbol),
                Some(margin.into_confidence()?),
            ),
            pipeline: Pipeline::Lexical,
            source: None,
        });
    }

    let candidates: SmallVec<[Candidate; RESULT_LIMIT]> = ranked
        .iter()
        .take(usize::from(profile.result_limit))
        .map(|entry| Candidate::new(TargetId::Symbol(entry.symbol), None))
        .collect();
    Ok(QueryResult::Candidates {
        candidates,
        pipeline: Pipeline::Lexical,
    })
}

/// Routes an exact miss through the lexical pipeline.
///
/// # Errors
/// Propagates every [`rank`] and [`decide`] failure.
pub fn route_lexical(
    snapshot: &Snapshot,
    query: &QueryTerms,
    profile: &RankingProfile,
    scratch: &mut RankingScratch,
) -> Result<(QueryResult, RankingEvidence), RankingError> {
    let (ranked, evidence) = rank(snapshot, query, profile, scratch)?;
    let result = decide(ranked, profile)?;
    Ok((result, evidence))
}

const fn none(reason: NoneReason) -> QueryResult {
    QueryResult::None {
        reason,
        pipeline: Pipeline::Lexical,
    }
}

/// Copies the query terms into ascending `TermId` order.
///
/// Scoring sums in this order, which is what makes a permuted query produce a
/// byte-identical score. `QueryTerms` is already deduplicated by [`crate::lex`];
/// this proves it instead of deduplicating a second time.
fn load_query_terms(query: &QueryTerms, scratch: &mut RankingScratch) -> Result<(), RankingError> {
    if u16::try_from(query.len()).is_err() {
        return Err(RankingError::QueryTooLarge { terms: query.len() });
    }
    scratch.terms.extend(query.iter());
    scratch.terms.sort_unstable();
    if scratch.terms.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(RankingError::InvalidQuery {
            reason: "query terms are not deduplicated",
        });
    }
    scratch.seen.resize(scratch.terms.len(), false);
    Ok(())
}

/// Opens one posting stream per `(query term, field)` pair that has postings.
///
/// Streams are opened in `(TermId ascending, field declaration order)`, which
/// fixes the stream indices used as the merge tie-break.
fn open_streams(snapshot: &Snapshot, scratch: &mut RankingScratch) -> Result<u16, RankingError> {
    let mut effective = 0u16;
    for (slot, &term) in scratch.terms.iter().enumerate() {
        let text = term_text(snapshot, term)?;
        let mut opened = false;
        for field in LexicalField::ALL {
            let lists = snapshot.postings.lists(field);
            let Some(list) = find_posting_list(snapshot, lists, term, text)? else {
                scratch.stream_of.push(NO_STREAM);
                continue;
            };
            let length = length_of(&lists[list])?;
            let stats = snapshot.corpus.field(field);
            if length > stats.document_count {
                return Err(RankingError::InvalidCorpusStats {
                    reason: "document frequency exceeds the field document count",
                });
            }
            let index =
                u32::try_from(scratch.streams.len()).map_err(|_| RankingError::QueryTooLarge {
                    terms: scratch.terms.len(),
                })?;
            scratch.stream_of.push(index);
            scratch.streams.push(Stream {
                field,
                term_slot: slot_index(slot)?,
                list: list_index(list)?,
                cursor: 0,
                length,
                idf: inverse_document_frequency(stats.document_count, length)?,
            });
            opened = true;
        }
        if opened {
            effective += 1;
        }
    }
    Ok(effective)
}

/// `ln(1 + (N - df + 0.5) / (df + 0.5))`, Lucene's non-negative BM25 IDF.
fn inverse_document_frequency(
    document_count: u32,
    document_frequency: u32,
) -> Result<f64, RankingError> {
    let numerator = f64::from(document_count) - f64::from(document_frequency) + 0.5;
    let denominator = f64::from(document_frequency) + 0.5;
    let idf = (1.0 + numerator / denominator).ln();
    if !idf.is_finite() || idf < 0.0 {
        return Err(RankingError::Arithmetic {
            reason: "inverse document frequency is not finite and non-negative",
        });
    }
    Ok(idf)
}

/// Merges the open streams by symbol id and retains the best union members.
///
/// The union is never materialized: only the bounded retention heap and the
/// per-symbol stream group are held at once.
///
/// [`RankingEvidence::cap_cut_a_tie`] is decided exactly while holding only the
/// single best member the cap discarded. Every discarded member ranks at or
/// below the retention root of the moment it was discarded, and that root only
/// improves as the merge proceeds, so every discarded member ranks at or below
/// the final root. The best discarded member therefore ties the final root
/// exactly when some discarded member does.
fn merge_candidates(
    snapshot: &Snapshot,
    profile: &RankingProfile,
    scratch: &mut RankingScratch,
    evidence: &mut RankingEvidence,
) -> Result<(), RankingError> {
    let RankingScratch {
        streams,
        merge,
        group,
        seen,
        retained,
        ..
    } = scratch;
    for (index, stream) in streams.iter().enumerate() {
        let symbol = head_symbol(snapshot, stream)?;
        merge.push(Reverse(StreamHead {
            symbol,
            stream: list_index(index)?,
        }));
    }

    let limit = usize::from(profile.candidate_limit);
    let mut best_discarded: Option<Retained> = None;
    while let Some(Reverse(head)) = merge.peek().copied() {
        let symbol = head.symbol;
        group.clear();
        while let Some(Reverse(next)) = merge.peek().copied() {
            if next.symbol != symbol {
                break;
            }
            merge.pop();
            group.push(next.stream);
        }

        let mut rarest_df = u32::MAX;
        let mut matched_terms = 0u16;
        for &index in group.iter() {
            let stream = stream_at(streams, index)?;
            rarest_df = rarest_df.min(stream.length);
            let slot = usize::from(stream.term_slot);
            if !seen[slot] {
                seen[slot] = true;
                matched_terms += 1;
            }
            evidence.posting_hits_scanned += 1;
        }
        for &index in group.iter() {
            seen[usize::from(stream_at(streams, index)?.term_slot)] = false;
        }
        evidence.candidates_before_cap += 1;
        let discarded = retain(
            retained,
            limit,
            Retained {
                rarest_df,
                matched_terms,
                symbol,
            },
        );
        if let Some(discarded) = discarded {
            best_discarded = Some(best_discarded.map_or(discarded, |best| best.min(discarded)));
        }

        for &index in group.iter() {
            let stream = streams
                .get_mut(index as usize)
                .ok_or(RankingError::InvalidPostings {
                    reason: "merge referenced an unknown stream",
                })?;
            stream.cursor += 1;
            if stream.cursor < stream.length {
                let next = head_symbol(snapshot, stream)?;
                if next <= symbol {
                    return Err(RankingError::InvalidPostings {
                        reason: "posting symbols are not strictly increasing",
                    });
                }
                merge.push(Reverse(StreamHead {
                    symbol: next,
                    stream: index,
                }));
            }
        }
    }

    evidence.cap_cut_a_tie = match (best_discarded, retained.peek()) {
        (Some(discarded), Some(worst_kept)) => discarded.signal() == worst_kept.signal(),
        _ => false,
    };
    Ok(())
}

/// Inserts a union member into the fixed-capacity retention heap.
///
/// The heap orders by the retention key, so its root is the worst member held
/// and is the only one a new arrival can displace.
///
/// Returns the member this call discarded, which is the arriving candidate when
/// it cannot displace the root, and the root itself when it can. Below the
/// limit nothing is discarded.
fn retain(heap: &mut BinaryHeap<Retained>, limit: usize, candidate: Retained) -> Option<Retained> {
    if heap.len() < limit {
        heap.push(candidate);
        return None;
    }
    let root = *heap.peek()?;
    if candidate < root {
        heap.pop();
        heap.push(candidate);
    }
    Some(root.max(candidate))
}

/// Scores every retained symbol and sorts by `(Score descending, SymbolId ascending)`.
fn score_retained(
    snapshot: &Snapshot,
    profile: &RankingProfile,
    scratch: &mut RankingScratch,
    evidence: &mut RankingEvidence,
) -> Result<(), RankingError> {
    let mut average_lengths = [0.0f64; LexicalField::COUNT];
    for field in LexicalField::ALL {
        average_lengths[field.index()] = snapshot.corpus.average_length(field)?.unwrap_or(0.0);
    }
    let scorer = Scorer {
        snapshot,
        profile,
        streams: &scratch.streams,
        stream_of: &scratch.stream_of,
        query_terms: scratch.terms.len(),
        average_lengths,
    };

    scratch.ranked.clear();
    for candidate in scratch.retained.drain() {
        scratch.ranked.push(RankedSymbol {
            symbol: candidate.symbol,
            score: scorer.score(candidate.symbol)?,
        });
    }
    scratch.ranked.sort_unstable_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    evidence.candidates_scored =
        u16::try_from(scratch.ranked.len()).map_err(|_| RankingError::Arithmetic {
            reason: "scored candidate count exceeds the evidence counter",
        })?;
    evidence.candidates_dropped = evidence
        .candidates_before_cap
        .saturating_sub(u64::from(evidence.candidates_scored));
    Ok(())
}

/// Per-call scoring context.
struct Scorer<'a> {
    snapshot: &'a Snapshot,
    profile: &'a RankingProfile,
    streams: &'a [Stream],
    stream_of: &'a [u32],
    query_terms: usize,
    /// Mean document length per field, zero for a field no symbol populates.
    ///
    /// An unpopulated field cannot own a posting list, so its length
    /// normalizer is never read and stays neutral instead of failing the query.
    average_lengths: [f64; LexicalField::COUNT],
}

impl Scorer<'_> {
    /// Sums `boost × idf × saturated tf` over query terms then fields, in that
    /// exact order, with compensated summation.
    ///
    /// Length normalization depends only on the field and the document, so it
    /// is computed once per document without disturbing the summation order.
    fn score(&self, symbol: SymbolId) -> Result<Score, RankingError> {
        let record =
            self.snapshot
                .symbols
                .get(symbol.index())
                .ok_or(RankingError::SymbolOutOfRange {
                    symbol: symbol.index(),
                    symbols: self.snapshot.symbols.len(),
                })?;

        let mut norms = [0.0f64; LexicalField::COUNT];
        for field in LexicalField::ALL {
            let params = self.profile.field(field);
            let average = self.average_lengths[field.index()];
            let normalized = if params.b == 0.0 || average <= 0.0 {
                1.0
            } else {
                1.0 - params.b + params.b * f64::from(record.field_lengths.get(field)) / average
            };
            norms[field.index()] = params.k1 * normalized;
        }

        let mut total = CompensatedSum::default();
        for slot in 0..self.query_terms {
            for field in LexicalField::ALL {
                let index = self.stream_of[slot * LexicalField::COUNT + field.index()];
                if index == NO_STREAM {
                    continue;
                }
                let stream = stream_at(self.streams, index)?;
                let Some(frequency) = self.term_frequency(stream, symbol)? else {
                    continue;
                };
                let params = self.profile.field(field);
                let frequency = f64::from(frequency);
                let saturated = frequency * (params.k1 + 1.0) / (frequency + norms[field.index()]);
                let contribution = params.boost * stream.idf * saturated;
                if !contribution.is_finite() || contribution < 0.0 {
                    return Err(RankingError::Arithmetic {
                        reason: "field contribution is not finite and non-negative",
                    });
                }
                total.add(contribution);
            }
        }
        Score::quantize(total.value())
    }

    /// Looks up `tf(term, field, symbol)` in an already-merged posting list.
    fn term_frequency(
        &self,
        stream: &Stream,
        symbol: SymbolId,
    ) -> Result<Option<u32>, RankingError> {
        let entries = posting_entries(self.snapshot, stream)?;
        let Ok(position) = entries.binary_search_by_key(&symbol, |entry| entry.symbol) else {
            return Ok(None);
        };
        let frequency = entries[position].term_frequency;
        if frequency == 0 {
            return Err(RankingError::InvalidPostings {
                reason: "posting carries a zero term frequency",
            });
        }
        Ok(Some(frequency))
    }
}

/// Neumaier compensated summation, so the declared order is the only thing that
/// determines the accumulated value.
#[derive(Debug, Clone, Copy, Default)]
struct CompensatedSum {
    total: f64,
    compensation: f64,
}

impl CompensatedSum {
    fn add(&mut self, value: f64) {
        let sum = self.total + value;
        self.compensation += if self.total.abs() >= value.abs() {
            (self.total - sum) + value
        } else {
            (value - sum) + self.total
        };
        self.total = sum;
    }

    fn value(self) -> f64 {
        self.total + self.compensation
    }
}

fn term_text(snapshot: &Snapshot, term: TermId) -> Result<&str, RankingError> {
    let record = snapshot
        .terms
        .get(term.index())
        .ok_or(RankingError::TermOutOfRange {
            term: term.index(),
            terms: snapshot.terms.len(),
        })?;
    snapshot
        .strings
        .get(record.text.index())
        .map(String::as_str)
        .ok_or(RankingError::InvalidPostings {
            reason: "term text is outside the string table",
        })
}

/// Finds a term's posting list inside one field.
///
/// Posting lists are ordered by canonical term text, so the search key is the
/// text; the recovered list must still carry the requested term id.
fn find_posting_list(
    snapshot: &Snapshot,
    lists: &[PostingList],
    term: TermId,
    text: &str,
) -> Result<Option<usize>, RankingError> {
    let found = lists.binary_search_by(|list| {
        list_text(snapshot, list)
            .unwrap_or_default()
            .as_bytes()
            .cmp(text.as_bytes())
    });
    let Ok(position) = found else {
        return Ok(None);
    };
    if lists[position].term != term {
        return Err(RankingError::InvalidPostings {
            reason: "posting list term disagrees with its canonical text",
        });
    }
    if lists[position].entries.is_empty() {
        return Err(RankingError::InvalidPostings {
            reason: "posting list is empty",
        });
    }
    Ok(Some(position))
}

fn list_text<'a>(snapshot: &'a Snapshot, list: &PostingList) -> Option<&'a str> {
    snapshot
        .terms
        .get(list.term.index())
        .and_then(|record| snapshot.strings.get(record.text.index()))
        .map(String::as_str)
}

fn posting_entries<'a>(
    snapshot: &'a Snapshot,
    stream: &Stream,
) -> Result<&'a [SymbolPosting], RankingError> {
    snapshot
        .postings
        .lists(stream.field)
        .get(stream.list as usize)
        .map(|list| list.entries.as_slice())
        .ok_or(RankingError::InvalidPostings {
            reason: "stream references an unknown posting list",
        })
}

fn head_symbol(snapshot: &Snapshot, stream: &Stream) -> Result<SymbolId, RankingError> {
    let entries = posting_entries(snapshot, stream)?;
    entries
        .get(stream.cursor as usize)
        .map(|entry| entry.symbol)
        .ok_or(RankingError::InvalidPostings {
            reason: "stream cursor is outside its posting list",
        })
}

fn stream_at(streams: &[Stream], index: u32) -> Result<&Stream, RankingError> {
    streams
        .get(index as usize)
        .ok_or(RankingError::InvalidPostings {
            reason: "reference to an unknown stream",
        })
}

fn length_of(list: &PostingList) -> Result<u32, RankingError> {
    u32::try_from(list.entries.len()).map_err(|_| RankingError::InvalidPostings {
        reason: "posting list is longer than the symbol id space",
    })
}

fn slot_index(slot: usize) -> Result<u16, RankingError> {
    u16::try_from(slot).map_err(|_| RankingError::QueryTooLarge { terms: slot })
}

fn list_index(index: usize) -> Result<u32, RankingError> {
    u32::try_from(index).map_err(|_| RankingError::InvalidPostings {
        reason: "index exceeds the addressable range",
    })
}

/// Widens an exact-range `u64` to `f64`.
fn widen_to_f64(value: u64) -> Result<f64, RankingError> {
    if value > (1u64 << 53) {
        return Err(RankingError::InvalidCorpusStats {
            reason: "corpus counter exceeds the exact integer range",
        });
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(value as f64)
}

/// Truncates a checked, non-negative, integral `f64` below 2^53 to `u64`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn truncate_to_u64(value: f64) -> u64 {
    debug_assert!((0.0..MAX_EXACT_INTEGER_F64).contains(&value));
    value as u64
}

/// Narrows a value already inside `0.0..=1.0` to `f32`.
#[allow(clippy::cast_possible_truncation)]
fn narrow_to_f32(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ranking_score_formats_six_fractional_digits() {
        assert_eq!(Score::from_millionths(12_345_678).to_string(), "12.345678");
        assert_eq!(Score::ZERO.to_string(), "0.000000");
        assert_eq!(Score::from_millionths(1).to_string(), "0.000001");
        assert_eq!(
            serde_json::to_string(&Score::from_millionths(12_345_678)).unwrap(),
            "\"12.345678\""
        );
    }

    #[test]
    fn ranking_score_quantization_rounds_half_up() {
        assert_eq!(Score::quantize(0.0).unwrap(), Score::ZERO);
        assert_eq!(
            Score::quantize(1.234_567_4).unwrap(),
            Score::from_millionths(1_234_567)
        );
        assert_eq!(
            Score::quantize(0.000_000_5).unwrap(),
            Score::from_millionths(1),
            "half a millionth rounds up"
        );
        assert_eq!(
            Score::quantize(0.000_000_4).unwrap(),
            Score::ZERO,
            "four tenths of a millionth rounds down"
        );
        assert_eq!(SCORE_SCALE.to_string(), "1000000");
        assert!((SCORE_SCALE_F64 - 1_000_000.0).abs() < f64::EPSILON);
        assert!((MARGIN_SCALE_F64 - f64::from(MARGIN_SCALE)).abs() < f64::EPSILON);
    }

    #[test]
    fn ranking_score_quantization_rejects_unrepresentable_values() {
        assert!(Score::quantize(f64::NAN).is_err());
        assert!(Score::quantize(f64::INFINITY).is_err());
        assert!(Score::quantize(-0.000_001).is_err());
        assert!(Score::quantize(1e300).is_err());
    }

    #[test]
    fn ranking_margin_uses_exact_integer_arithmetic() {
        let top = Score::from_millionths(10_000_000);
        assert_eq!(
            MarginPpm::between(top, Score::from_millionths(2_500_000)),
            MarginPpm(750_000)
        );
        assert_eq!(MarginPpm::between(top, top), MarginPpm::ZERO);
        assert_eq!(MarginPpm::between(top, Score::ZERO), MarginPpm::FULL);
        assert_eq!(
            MarginPpm::between(Score::ZERO, Score::ZERO),
            MarginPpm::ZERO
        );
        assert_eq!(
            MarginPpm::between(Score::from_millionths(u64::MAX), Score::ZERO),
            MarginPpm::FULL
        );
        assert!(MarginPpm::new(MARGIN_SCALE + 1).is_err());
    }

    #[test]
    fn ranking_margin_converts_into_confidence() {
        assert!((MarginPpm::FULL.into_confidence().unwrap().get() - 1.0).abs() < f32::EPSILON);
        assert!(MarginPpm::ZERO.into_confidence().unwrap().get().abs() < f32::EPSILON);
        assert!((MarginPpm(250_000).into_confidence().unwrap().get() - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn ranking_default_profile_matches_the_declared_table() {
        let expected = [
            (LexicalField::Name, 8.0, 1.2, 0.00),
            (LexicalField::Qualified, 5.0, 1.2, 0.30),
            (LexicalField::Path, 5.0, 1.2, 0.30),
            (LexicalField::Signature, 4.0, 1.2, 0.75),
            (LexicalField::Body, 1.5, 1.2, 0.75),
            (LexicalField::Documentation, 2.0, 1.2, 0.75),
            (LexicalField::Attribute, 1.0, 1.2, 0.50),
            (LexicalField::Callee, 3.0, 1.2, 0.50),
            (LexicalField::Caller, 3.0, 1.2, 0.50),
            (LexicalField::Import, 2.0, 1.2, 0.50),
        ];
        for (field, boost, k1, b) in expected {
            let params = DEFAULT_RANKING_PROFILE.field(field);
            assert!((params.boost - boost).abs() < f64::EPSILON, "{field} boost");
            assert!((params.k1 - k1).abs() < f64::EPSILON, "{field} k1");
            assert!((params.b - b).abs() < f64::EPSILON, "{field} b");
        }
        DEFAULT_RANKING_PROFILE.validate().unwrap();
        assert_eq!(
            usize::from(DEFAULT_RANKING_PROFILE.candidate_limit),
            CANDIDATE_LIMIT
        );
        assert_eq!(
            usize::from(DEFAULT_RANKING_PROFILE.result_limit),
            RESULT_LIMIT
        );
    }

    #[test]
    fn ranking_profile_validation_rejects_every_invalid_parameter() {
        type Mutation = fn(&mut RankingProfile);
        let cases: [(&str, Mutation); 8] = [
            ("negative boost", |p| p.fields[0].boost = -1.0),
            ("non-finite boost", |p| p.fields[3].boost = f64::NAN),
            ("negative k1", |p| p.fields[1].k1 = -0.1),
            ("b above one", |p| p.fields[2].b = 1.5),
            ("direct answer below the abstention floor", |p| {
                p.thresholds.none_below = Score::from_millionths(10);
                p.thresholds.direct_at_least = Score::from_millionths(9);
            }),
            ("zero candidate limit", |p| p.candidate_limit = 0),
            ("limits inverted", |p| {
                p.candidate_limit = 2;
                p.result_limit = 3;
            }),
            ("result limit beyond contract", |p| {
                p.candidate_limit = 64;
                p.result_limit = 4;
            }),
        ];
        for (label, mutate) in cases {
            let mut profile = DEFAULT_RANKING_PROFILE;
            mutate(&mut profile);
            assert!(
                matches!(profile.validate(), Err(RankingError::InvalidProfile { .. })),
                "{label} must be rejected"
            );
        }
    }

    #[test]
    fn ranking_decide_rejects_an_invalid_profile() {
        let mut profile = DEFAULT_RANKING_PROFILE;
        profile.result_limit = 4;
        assert!(
            matches!(
                decide(&[], &profile),
                Err(RankingError::InvalidProfile { .. })
            ),
            "a public entry point must reject a profile it cannot honour, \
             even on the empty ranking that answers without reading it"
        );
    }

    #[test]
    fn ranking_scoring_digest_ignores_thresholds_only() {
        let baseline = DEFAULT_RANKING_PROFILE.scoring_digest();

        let mut retuned = DEFAULT_RANKING_PROFILE;
        retuned.thresholds.direct_at_least = Score::from_millionths(1);
        retuned.thresholds.none_below = Score::from_millionths(2);
        retuned.thresholds.direct_margin_at_least = MarginPpm(3);
        assert_eq!(retuned.scoring_digest(), baseline);
        assert!(RankingStamp::of(&DEFAULT_RANKING_PROFILE).accepts(&retuned));

        for mutate in [
            (|p: &mut RankingProfile| p.fields[4].boost = 1.6) as fn(&mut RankingProfile),
            |p: &mut RankingProfile| p.fields[0].k1 = 1.3,
            |p: &mut RankingProfile| p.fields[9].b = 0.4,
            |p: &mut RankingProfile| p.version += 1,
            |p: &mut RankingProfile| p.candidate_limit = 32,
            |p: &mut RankingProfile| p.result_limit = 2,
        ] {
            let mut profile = DEFAULT_RANKING_PROFILE;
            mutate(&mut profile);
            assert_ne!(profile.scoring_digest(), baseline);
            assert!(!RankingStamp::of(&DEFAULT_RANKING_PROFILE).accepts(&profile));
        }
    }

    #[test]
    fn ranking_idf_is_larger_for_rarer_terms() {
        let rare = inverse_document_frequency(1000, 1).unwrap();
        let common = inverse_document_frequency(1000, 500).unwrap();
        let ubiquitous = inverse_document_frequency(1000, 1000).unwrap();
        assert!(rare > common, "{rare} > {common}");
        assert!(common > ubiquitous, "{common} > {ubiquitous}");
        assert!(
            ubiquitous > 0.0 && ubiquitous.is_finite(),
            "a term in every document keeps a small positive weight"
        );
        let single = inverse_document_frequency(1, 1).unwrap();
        assert!(
            (single - (4.0f64 / 3.0).ln()).abs() < 1e-12,
            "ln(1 + (1 - 1 + 0.5) / 1.5) is ln(4/3), not {single}"
        );
    }

    #[test]
    fn ranking_average_length_reports_empty_and_inconsistent_fields() {
        let mut stats = CorpusStats::EMPTY;
        assert_eq!(stats.average_length(LexicalField::Name).unwrap(), None);

        stats.fields[LexicalField::Name.index()] = FieldStats {
            document_count: 4,
            total_term_frequency: 10,
        };
        assert_eq!(stats.average_length(LexicalField::Name).unwrap(), Some(2.5));

        stats.fields[LexicalField::Body.index()] = FieldStats {
            document_count: 0,
            total_term_frequency: 3,
        };
        assert!(matches!(
            stats.average_length(LexicalField::Body),
            Err(RankingError::InvalidCorpusStats { .. })
        ));

        stats.fields[LexicalField::Body.index()] = FieldStats {
            document_count: 4,
            total_term_frequency: 3,
        };
        assert!(matches!(
            stats.average_length(LexicalField::Body),
            Err(RankingError::InvalidCorpusStats { .. })
        ));
    }

    #[test]
    fn ranking_compensated_sum_recovers_small_contributions() {
        let mut naive = 1.0f64;
        let mut compensated = CompensatedSum::default();
        compensated.add(1.0);
        for _ in 0..20 {
            naive += 1e-17;
            compensated.add(1e-17);
        }
        assert!(
            (naive - 1.0).abs() < f64::EPSILON / 2.0,
            "naive accumulation drops every contribution below the ulp of 1.0"
        );
        assert!(compensated.value() > 1.0, "compensation must survive");
        assert!(compensated.value() < 1.000_000_1);
    }

    #[test]
    fn ranking_retention_key_orders_best_first() {
        let best = Retained {
            rarest_df: 1,
            matched_terms: 2,
            symbol: SymbolId::from_index(9),
        };
        let by_df = Retained {
            rarest_df: 2,
            matched_terms: 2,
            symbol: SymbolId::from_index(0),
        };
        let by_matched = Retained {
            rarest_df: 1,
            matched_terms: 1,
            symbol: SymbolId::from_index(0),
        };
        let by_symbol = Retained {
            rarest_df: 1,
            matched_terms: 2,
            symbol: SymbolId::from_index(10),
        };
        assert!(best < by_df, "rarer document frequency wins");
        assert!(best < by_matched, "more matched terms wins");
        assert!(best < by_symbol, "lower symbol id wins");
    }

    #[test]
    fn ranking_retention_heap_keeps_the_best_members() {
        let mut heap = BinaryHeap::new();
        for id in 0..10u32 {
            retain(
                &mut heap,
                3,
                Retained {
                    rarest_df: 10 - id,
                    matched_terms: 1,
                    symbol: SymbolId::from_index(id),
                },
            );
        }
        let mut kept: Vec<u32> = heap
            .drain()
            .map(|entry| entry.rarest_df)
            .collect::<Vec<_>>();
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 2, 3]);
    }

    #[test]
    fn ranking_retention_returns_the_member_it_discarded() {
        let member = |rarest_df: u32, id: u32| Retained {
            rarest_df,
            matched_terms: 1,
            symbol: SymbolId::from_index(id),
        };
        let mut heap = BinaryHeap::new();
        assert_eq!(
            retain(&mut heap, 1, member(5, 0)),
            None,
            "a heap below its limit discards nothing"
        );
        assert_eq!(
            retain(&mut heap, 1, member(9, 1)),
            Some(member(9, 1)),
            "a member that cannot displace the root is the one discarded"
        );
        assert_eq!(
            retain(&mut heap, 1, member(2, 2)),
            Some(member(5, 0)),
            "a member that displaces the root discards the root"
        );
    }
}

//! Offline calibration of the lexical decision thresholds.
//!
//! The corpus is a set of natural-language questions asked against fixture
//! repositories, each labelled with the answer a router should give and the
//! anchors that answer may name. Thresholds are fitted by exhaustive search
//! over the boundaries the corpus itself observes, so the search is finite,
//! deterministic, and free of tuning constants.
//!
//! The abstention floor is fitted first: every value that never abstains on a
//! case with acceptable anchors, keeping the one that rejects the most
//! unanswerable cases. The direct thresholds are fitted next, over the pairs
//! that never answer a `candidates` or `none` case and never name an anchor
//! outside the acceptable set, keeping the pair that answers the most.
//!
//! Where a whole range of values shares the best result this takes the range
//! midpoint rather than its largest value. The largest value always sits
//! exactly on a case the corpus contains, so a held-out repository whose
//! scores fall just below it loses every answer; the midpoint keeps each
//! threshold as far from the cases that pin it as the corpus allows. That is
//! what lets the folds below hold their recall.
//!
//! Generalization is measured by holding out one whole repository at a time:
//! a fold fits on the other four and is scored on the untouched one. The
//! shipped thresholds are the all-corpus fit; the folds report what that fit
//! costs on a repository it has never seen. Every repository therefore carries
//! cases of every class: a class that lives in one repository alone cannot be
//! cross-validated, because the fold holding that repository out fits
//! thresholds that have never seen the class.
//!
//! Labels describe the query the router actually receives, which is the query
//! left after normalization drops stop words and words no repository term
//! matches. A case is unanswerable when the surviving terms name nothing the
//! repository defines: either every content word is unknown, or the survivors
//! appear only as incidental vocabulary in signatures, bodies, and prose.
//! Calling a sentence unanswerable while one surviving term names a type would
//! ask the ranker to weigh words it never received.
//!
//! Regenerate the report with `RR_UPDATE_CALIBRATION=1`.

#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use rr_core::index::Snapshot;
use rr_core::query::{parse_query, route_exact, ExactOutcome, QueryRequest};
use rr_core::ranking::{
    decide, rank, DecisionThresholds, MarginPpm, RankingScratch, Score, DEFAULT_RANKING_PROFILE,
};
use rr_core::result::QueryResult;
use serde_json::{json, Value};
use support::{anchor_of, fixture_snapshot, FIXTURE_REPOSITORIES};

/// Minimum share of answerable cases whose acceptable anchor must survive in
/// the first three results of a held-out fold, in parts per million.
const REQUIRED_RECALL_PPM: u64 = 900_000;

/// Fixed-point scale shared by every ratio the report emits.
const PPM: u64 = 1_000_000;

/// The answer a router should give for one corpus case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Label {
    /// One anchor is justified: exactly the anchors listed are acceptable.
    Direct,
    /// The evidence is ambiguous: a candidate list is the honest answer.
    Candidates,
    /// The repository cannot answer the question at all.
    None,
}

impl Label {
    fn parse(text: &str) -> Self {
        match text {
            "direct" => Self::Direct,
            "candidates" => Self::Candidates,
            "none" => Self::None,
            other => panic!("unknown corpus label {other}"),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Candidates => "candidates",
            Self::None => "none",
        }
    }

    const fn is_answerable(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// One labelled corpus case.
struct Case {
    id: String,
    repository: String,
    query: String,
    label: Label,
    anchors: Vec<String>,
}

/// Everything the threshold search needs from one ranked query.
///
/// Ranking does not depend on thresholds, so each case is ranked once and the
/// search replays these summaries instead of re-ranking.
struct Observation {
    ranked: bool,
    top_score: Score,
    margin: MarginPpm,
    top_anchor: String,
    /// One-based position of the first acceptable anchor within the results a
    /// candidate answer would carry, when the case has one.
    acceptable_position: Option<u32>,
}

impl Observation {
    const fn top_anchor_acceptable(&self) -> bool {
        matches!(self.acceptable_position, Some(1))
    }

    const fn covered(&self) -> bool {
        self.acceptable_position.is_some()
    }
}

/// The decision a threshold triple produces for one observation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decision {
    Direct,
    Candidates,
    None,
}

impl Decision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Candidates => "candidates",
            Self::None => "none",
        }
    }

    const fn abstains(self) -> bool {
        matches!(self, Self::None)
    }
}

/// How a decision scored against the label of its case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// A direct answer naming an acceptable anchor for a direct case.
    CorrectDirect,
    /// A direct answer a downstream agent would follow into the wrong place,
    /// or into a single place where no single place is justified.
    WrongDirect,
    /// A candidate list holding an acceptable anchor.
    CoveredCandidates,
    /// A candidate list that lost every acceptable anchor, or one offered
    /// where abstention was the right answer.
    UncoveredCandidates,
    /// Abstention on an unanswerable case.
    CorrectNone,
    /// Abstention on an answerable case.
    MissedAnswer,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CorrectDirect => "correct_direct",
            Self::WrongDirect => "wrong_direct",
            Self::CoveredCandidates => "covered_candidates",
            Self::UncoveredCandidates => "uncovered_candidates",
            Self::CorrectNone => "correct_none",
            Self::MissedAnswer => "missed_answer",
        }
    }
}

/// Counts scored over one case set.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Metrics {
    answerable: u32,
    correct_directs: u32,
    wrong_directs: u32,
    correct_nones: u32,
    covered: u32,
    top_one: u32,
    reciprocal_rank_total_ppm: u64,
    unanswerable_abstained: u32,
    unanswerable_answered: u32,
    answerable_abstained: u32,
    answerable_answered: u32,
}

impl Metrics {
    fn add(&mut self, case: &Case, observation: &Observation, decision: Decision) {
        let outcome = judge(case, observation, decision);
        match (case.label.is_answerable(), decision.abstains()) {
            (true, true) => {
                self.answerable += 1;
                self.answerable_abstained += 1;
            }
            (true, false) => {
                self.answerable += 1;
                self.answerable_answered += 1;
            }
            (false, true) => self.unanswerable_abstained += 1,
            (false, false) => self.unanswerable_answered += 1,
        }
        match outcome {
            Outcome::CorrectDirect => {
                self.correct_directs += 1;
                self.covered += 1;
            }
            Outcome::WrongDirect => self.wrong_directs += 1,
            Outcome::CoveredCandidates => self.covered += 1,
            Outcome::CorrectNone => self.correct_nones += 1,
            Outcome::UncoveredCandidates | Outcome::MissedAnswer => {}
        }
        if !case.label.is_answerable() || decision.abstains() {
            return;
        }
        let position = match decision {
            Decision::Direct => u32::from(observation.top_anchor_acceptable()),
            Decision::Candidates => observation.acceptable_position.unwrap_or_default(),
            Decision::None => 0,
        };
        if position == 1 {
            self.top_one += 1;
        }
        if position > 0 {
            self.reciprocal_rank_total_ppm += PPM / u64::from(position);
        }
    }

    /// How often the fit answers correctly, which is what it maximizes.
    const fn correct_directs(&self) -> u32 {
        self.correct_directs
    }

    fn recall_ppm(&self) -> u64 {
        ratio_ppm(u64::from(self.covered), u64::from(self.answerable))
    }

    fn top_one_ppm(&self) -> u64 {
        ratio_ppm(u64::from(self.top_one), u64::from(self.answerable))
    }

    fn mean_reciprocal_rank_ppm(&self) -> u64 {
        if self.answerable == 0 {
            return 0;
        }
        self.reciprocal_rank_total_ppm / u64::from(self.answerable)
    }
}

fn ratio_ppm(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return PPM;
    }
    part * PPM / whole
}

/// A complete calibration run: the shipped fit plus one held-out fold per
/// fixture repository.
struct Calibration {
    cases: Vec<Case>,
    observations: Vec<Observation>,
    digest: String,
    shipped: DecisionThresholds,
    shipped_metrics: Metrics,
    folds: Vec<Fold>,
}

struct Fold {
    held_out: String,
    thresholds: DecisionThresholds,
    metrics: Metrics,
    members: Vec<usize>,
}

fn calibration() -> &'static Calibration {
    static CALIBRATION: OnceLock<Calibration> = OnceLock::new();
    CALIBRATION.get_or_init(run_calibration)
}

fn run_calibration() -> Calibration {
    let (cases, digest) = load_cases();
    let mut snapshots: BTreeMap<&str, Snapshot> = BTreeMap::new();
    for repository in FIXTURE_REPOSITORIES {
        snapshots.insert(repository, fixture_snapshot(repository));
    }

    let mut scratch = RankingScratch::new();
    let mut observations = Vec::with_capacity(cases.len());
    for case in &cases {
        let snapshot = snapshots
            .get(case.repository.as_str())
            .unwrap_or_else(|| panic!("case {} names an unknown repository", case.id));
        observations.push(observe(snapshot, case, &mut scratch));
    }

    let all: Vec<usize> = (0..cases.len()).collect();
    let shipped = fit(&cases, &observations, &all);
    let shipped_metrics = score(&cases, &observations, &all, &shipped);

    let mut folds = Vec::with_capacity(FIXTURE_REPOSITORIES.len());
    for repository in FIXTURE_REPOSITORIES {
        let members: Vec<usize> = all
            .iter()
            .copied()
            .filter(|index| cases[*index].repository == repository)
            .collect();
        let fitted: Vec<usize> = all
            .iter()
            .copied()
            .filter(|index| cases[*index].repository != repository)
            .collect();
        assert!(
            !members.is_empty(),
            "fixture repository {repository} has no corpus case"
        );
        let thresholds = fit(&cases, &observations, &fitted);
        let metrics = score(&cases, &observations, &members, &thresholds);
        folds.push(Fold {
            held_out: repository.to_string(),
            thresholds,
            metrics,
            members,
        });
    }

    Calibration {
        cases,
        observations,
        digest,
        shipped,
        shipped_metrics,
        folds,
    }
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ranking/cases.jsonl")
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ranking/calibration.expected.json")
}

/// Reads the corpus and the digest of the exact bytes that produced this run.
fn load_cases() -> (Vec<Case>, String) {
    let raw = std::fs::read(corpus_path()).expect("read the calibration corpus");
    let digest = blake3::hash(&raw).to_hex().to_string();
    let raw = String::from_utf8(raw).expect("the corpus is UTF-8");
    let mut cases: Vec<Case> = Vec::new();
    for line in raw.lines() {
        let value: Value = serde_json::from_str(line).expect("parse a corpus case");
        let anchors = value["anchors"]
            .as_array()
            .expect("corpus anchors are an array")
            .iter()
            .map(|anchor| anchor.as_str().expect("an anchor is a string").to_string())
            .collect::<Vec<_>>();
        let label = Label::parse(value["label"].as_str().expect("a label is a string"));
        assert_eq!(
            label == Label::None,
            anchors.is_empty(),
            "case {} pairs its label with the wrong anchor set",
            value["id"]
        );
        cases.push(Case {
            id: value["id"].as_str().expect("an id is a string").to_string(),
            repository: value["repository"]
                .as_str()
                .expect("a repository is a string")
                .to_string(),
            query: value["query"]
                .as_str()
                .expect("a query is a string")
                .to_string(),
            label,
            anchors,
        });
    }
    assert!(
        cases.windows(2).all(|pair| pair[0].id < pair[1].id),
        "corpus cases must be unique and sorted by id"
    );
    assert!(
        cases.len() >= 20,
        "the corpus must hold at least twenty cases"
    );
    (cases, digest)
}

/// Ranks one case and summarizes everything the search needs.
fn observe(snapshot: &Snapshot, case: &Case, scratch: &mut RankingScratch) -> Observation {
    let parsed = parse_query(snapshot, QueryRequest::new(&case.query, None))
        .unwrap_or_else(|error| panic!("case {} does not parse: {error}", case.id));
    assert!(
        matches!(route_exact(snapshot, &parsed), ExactOutcome::Miss),
        "case {} is answered by the exact router, so it cannot calibrate the lexical fallback",
        case.id
    );
    for anchor in &case.anchors {
        assert!(
            snapshot
                .symbols
                .iter()
                .any(|symbol| anchor_of(snapshot, symbol.id) == *anchor),
            "case {} names anchor {anchor}, which its repository does not contain",
            case.id
        );
    }

    let (ranked, _evidence) = rank(snapshot, &parsed.terms, &DEFAULT_RANKING_PROFILE, scratch)
        .unwrap_or_else(|error| panic!("case {} does not rank: {error}", case.id));
    let results: Vec<String> = ranked
        .iter()
        .take(usize::from(DEFAULT_RANKING_PROFILE.result_limit))
        .map(|entry| anchor_of(snapshot, entry.symbol))
        .collect();
    let top_score = ranked.first().map_or(Score::ZERO, |entry| entry.score);
    let runner_up = ranked.get(1).map_or(Score::ZERO, |entry| entry.score);

    Observation {
        ranked: !ranked.is_empty(),
        top_score,
        margin: MarginPpm::between(top_score, runner_up),
        acceptable_position: results
            .iter()
            .position(|anchor| case.anchors.contains(anchor))
            .map(|index| u32::try_from(index).expect("a position fits") + 1),
        top_anchor: results.first().cloned().unwrap_or_default(),
    }
}

/// Mirrors [`rr_core::decide`] over a pre-ranked observation.
///
/// `ranking_calibration_matches_the_shipped_decision` pins the two together.
fn classify(observation: &Observation, thresholds: &DecisionThresholds) -> Decision {
    if !observation.ranked || observation.top_score < thresholds.none_below {
        Decision::None
    } else if observation.top_score >= thresholds.direct_at_least
        && observation.margin >= thresholds.direct_margin_at_least
    {
        Decision::Direct
    } else {
        Decision::Candidates
    }
}

fn judge(case: &Case, observation: &Observation, decision: Decision) -> Outcome {
    match decision {
        Decision::Direct => {
            if case.label == Label::Direct && observation.top_anchor_acceptable() {
                Outcome::CorrectDirect
            } else {
                Outcome::WrongDirect
            }
        }
        Decision::Candidates => {
            if case.label.is_answerable() && observation.covered() {
                Outcome::CoveredCandidates
            } else {
                Outcome::UncoveredCandidates
            }
        }
        Decision::None => {
            if case.label.is_answerable() {
                Outcome::MissedAnswer
            } else {
                Outcome::CorrectNone
            }
        }
    }
}

fn score(
    cases: &[Case],
    observations: &[Observation],
    members: &[usize],
    thresholds: &DecisionThresholds,
) -> Metrics {
    let mut metrics = Metrics::default();
    for &index in members {
        let decision = classify(&observations[index], thresholds);
        metrics.add(&cases[index], &observations[index], decision);
    }
    metrics
}

fn fit(cases: &[Case], observations: &[Observation], members: &[usize]) -> DecisionThresholds {
    let none_below = fit_none_below(cases, observations, members);
    let (direct_at_least, direct_margin_at_least) =
        fit_direct(cases, observations, members, none_below);
    DecisionThresholds {
        none_below,
        direct_at_least,
        direct_margin_at_least,
    }
}

/// Fits the abstention floor.
///
/// Only a value the corpus observes can move a decision, so the search walks
/// every observed top score and its successor: one answers that case, the
/// other abstains from it. Feasible values never abstain on a case that has an
/// acceptable anchor; among the feasible values that reject the most
/// unanswerable cases, the range midpoint is taken.
fn fit_none_below(cases: &[Case], observations: &[Observation], members: &[usize]) -> Score {
    let mut best_rejected = 0;
    let mut lowest = None;
    let mut highest = Score::ZERO;
    for value in boundary_scores(observations, members) {
        let mut rejected = 0;
        let mut abstains_on_an_answer = false;
        for &index in members {
            let observation = &observations[index];
            let abstains = !observation.ranked || observation.top_score < value;
            if cases[index].label.is_answerable() {
                abstains_on_an_answer |= abstains;
            } else if abstains {
                rejected += 1;
            }
        }
        if abstains_on_an_answer {
            continue;
        }
        if lowest.is_none() || rejected > best_rejected {
            best_rejected = rejected;
            lowest = Some(value);
            highest = value;
        } else if rejected == best_rejected {
            highest = value;
        }
    }
    let lowest = lowest.expect("zero always abstains on nothing");
    middle_score(lowest, highest)
}

/// Fits the direct score and margin thresholds above a fitted floor.
///
/// A pair is rejected when it answers a `candidates` or `none` case directly,
/// or names an anchor outside the acceptable set. Among the pairs that answer
/// the most cases correctly, the component midpoints are taken when they hold
/// the same result; otherwise the issue's tie-break applies, which is the
/// largest margin and then the largest absolute threshold.
fn fit_direct(
    cases: &[Case],
    observations: &[Observation],
    members: &[usize],
    none_below: Score,
) -> (Score, MarginPpm) {
    let scores: Vec<Score> = boundary_scores(observations, members)
        .into_iter()
        .filter(|value| *value >= none_below)
        .collect();
    let margins = boundary_margins(observations, members);

    let mut best = 0;
    let mut bounds: Option<(Score, Score, MarginPpm, MarginPpm)> = None;
    let mut tie_break = (Score::ZERO, MarginPpm::ZERO);
    for &direct_at_least in &scores {
        for &direct_margin_at_least in &margins {
            let thresholds = DecisionThresholds {
                none_below,
                direct_at_least,
                direct_margin_at_least,
            };
            let metrics = score(cases, observations, members, &thresholds);
            if metrics.wrong_directs > 0 {
                continue;
            }
            let correct = metrics.correct_directs();
            if bounds.is_none() || correct > best {
                best = correct;
                bounds = Some((
                    direct_at_least,
                    direct_at_least,
                    direct_margin_at_least,
                    direct_margin_at_least,
                ));
                tie_break = (direct_at_least, direct_margin_at_least);
            } else if correct == best {
                let (low_score, high_score, low_margin, high_margin) =
                    bounds.expect("a best result always carries its bounds");
                bounds = Some((
                    low_score.min(direct_at_least),
                    high_score.max(direct_at_least),
                    low_margin.min(direct_margin_at_least),
                    high_margin.max(direct_margin_at_least),
                ));
                if (direct_margin_at_least, direct_at_least) > (tie_break.1, tie_break.0) {
                    tie_break = (direct_at_least, direct_margin_at_least);
                }
            }
        }
    }

    let (low_score, high_score, low_margin, high_margin) =
        bounds.expect("abstaining on everything is always feasible");
    let middle = (
        middle_score(low_score, high_score),
        middle_margin(low_margin, high_margin),
    );
    let metrics = score(
        cases,
        observations,
        members,
        &DecisionThresholds {
            none_below,
            direct_at_least: middle.0,
            direct_margin_at_least: middle.1,
        },
    );
    if metrics.wrong_directs == 0 && metrics.correct_directs() == best {
        middle
    } else {
        tie_break
    }
}

fn middle_score(low: Score, high: Score) -> Score {
    Score::from_millionths(u64::midpoint(low.millionths(), high.millionths()))
}

fn middle_margin(low: MarginPpm, high: MarginPpm) -> MarginPpm {
    MarginPpm::new(u32::midpoint(low.get(), high.get())).expect("a midpoint of margins is a margin")
}

/// Every score that can move a decision boundary, plus zero.
fn boundary_scores(observations: &[Observation], members: &[usize]) -> Vec<Score> {
    let mut values = vec![Score::ZERO];
    for &index in members {
        let top = observations[index].top_score;
        values.push(top);
        values.push(Score::from_millionths(top.millionths().saturating_add(1)));
    }
    values.sort_unstable();
    values.dedup();
    values
}

/// Every margin that can move a decision boundary, plus zero.
fn boundary_margins(observations: &[Observation], members: &[usize]) -> Vec<MarginPpm> {
    let mut values = vec![MarginPpm::ZERO];
    for &index in members {
        let margin = observations[index].margin;
        values.push(margin);
        if let Ok(successor) = MarginPpm::new(margin.get().saturating_add(1)) {
            values.push(successor);
        }
    }
    values.sort_unstable();
    values.dedup();
    values
}

fn thresholds_json(thresholds: &DecisionThresholds) -> Value {
    json!({
        "none_below": thresholds.none_below.millionths(),
        "direct_at_least": thresholds.direct_at_least.millionths(),
        "direct_margin_at_least_ppm": thresholds.direct_margin_at_least.get(),
    })
}

fn metrics_json(metrics: &Metrics) -> Value {
    json!({
        "answerable": metrics.answerable,
        "correct_directs": metrics.correct_directs,
        "correct_nones": metrics.correct_nones,
        "covered": metrics.covered,
        "mean_reciprocal_rank_ppm": metrics.mean_reciprocal_rank_ppm(),
        "none_confusion": {
            "answerable_abstained": metrics.answerable_abstained,
            "answerable_answered": metrics.answerable_answered,
            "unanswerable_abstained": metrics.unanswerable_abstained,
            "unanswerable_answered": metrics.unanswerable_answered,
        },
        "top_one_recall_ppm": metrics.top_one_ppm(),
        "top_three_recall_ppm": metrics.recall_ppm(),
        "wrong_directs": metrics.wrong_directs,
    })
}

fn case_json(case: &Case, observation: &Observation, thresholds: &DecisionThresholds) -> Value {
    let decision = classify(observation, thresholds);
    json!({
        "decision": decision.as_str(),
        "id": case.id,
        "label": case.label.as_str(),
        "margin_ppm": observation.margin.get(),
        "outcome": judge(case, observation, decision).as_str(),
        "top_anchor": observation.top_anchor,
        "top_score": observation.top_score.to_string(),
    })
}

fn report(calibration: &Calibration) -> String {
    let cases: Vec<Value> = calibration
        .cases
        .iter()
        .zip(&calibration.observations)
        .map(|(case, observation)| case_json(case, observation, &calibration.shipped))
        .collect();
    let folds: Vec<Value> = calibration
        .folds
        .iter()
        .map(|fold| {
            let members: Vec<Value> = fold
                .members
                .iter()
                .map(|&index| {
                    case_json(
                        &calibration.cases[index],
                        &calibration.observations[index],
                        &fold.thresholds,
                    )
                })
                .collect();
            json!({
                "cases": members,
                "held_out": fold.held_out,
                "metrics": metrics_json(&fold.metrics),
                "thresholds": thresholds_json(&fold.thresholds),
            })
        })
        .collect();
    let document = json!({
        "cases": cases,
        "corpus": {
            "cases": calibration.cases.len(),
            "digest": calibration.digest,
            "repositories": FIXTURE_REPOSITORIES,
        },
        "folds": folds,
        "profile_version": DEFAULT_RANKING_PROFILE.version,
        "scoring_digest": hex(&DEFAULT_RANKING_PROFILE.scoring_digest()),
        "shipped": {
            "metrics": metrics_json(&calibration.shipped_metrics),
            "thresholds": thresholds_json(&calibration.shipped),
        },
    });
    let mut text = serde_json::to_string_pretty(&document).expect("serialize the report");
    text.push('\n');
    text
}

fn hex(bytes: &[u8; 32]) -> String {
    blake3::Hash::from(*bytes).to_hex().to_string()
}

fn summary(calibration: &Calibration) -> String {
    let mut text = String::new();
    writeln!(
        text,
        "corpus {} cases over {} repositories, digest {}",
        calibration.cases.len(),
        FIXTURE_REPOSITORIES.len(),
        calibration.digest
    )
    .expect("write the summary header");
    text.push_str("fold        none_below  direct_at_least  margin_ppm  directs  wrong  top3\n");
    for (name, thresholds, metrics) in calibration
        .folds
        .iter()
        .map(|fold| (fold.held_out.as_str(), fold.thresholds, fold.metrics))
        .chain(std::iter::once((
            "shipped",
            calibration.shipped,
            calibration.shipped_metrics,
        )))
    {
        writeln!(
            text,
            "{name:<10}  {:>10}  {:>15}  {:>10}  {:>7}  {:>5}  {:>4}%",
            thresholds.none_below.to_string(),
            thresholds.direct_at_least.to_string(),
            thresholds.direct_margin_at_least.get(),
            metrics.correct_directs,
            metrics.wrong_directs,
            metrics.recall_ppm() / 10_000
        )
        .expect("write a summary row");
    }
    text
}

#[test]
fn ranking_calibration_matches_default_profile() {
    let calibration = calibration();
    assert_eq!(
        DEFAULT_RANKING_PROFILE.thresholds, calibration.shipped,
        "the shipped thresholds are stale; rerun with RR_UPDATE_CALIBRATION=1 and paste \
         the fitted values into DEFAULT_RANKING_PROFILE"
    );
    DEFAULT_RANKING_PROFILE
        .validate()
        .expect("the shipped profile is valid");
}

#[test]
fn ranking_calibration_matches_the_shipped_decision() {
    let calibration = calibration();
    let mut snapshots: BTreeMap<&str, Snapshot> = BTreeMap::new();
    for repository in FIXTURE_REPOSITORIES {
        snapshots.insert(repository, fixture_snapshot(repository));
    }
    let mut scratch = RankingScratch::new();
    for (case, observation) in calibration.cases.iter().zip(&calibration.observations) {
        let snapshot = &snapshots[case.repository.as_str()];
        let parsed = parse_query(snapshot, QueryRequest::new(&case.query, None)).unwrap();
        let (ranked, _evidence) = rank(
            snapshot,
            &parsed.terms,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .unwrap();
        let decided = decide(ranked, &DEFAULT_RANKING_PROFILE).unwrap();
        let expected = classify(observation, &DEFAULT_RANKING_PROFILE.thresholds);
        let actual = match decided {
            QueryResult::Direct { .. } => Decision::Direct,
            QueryResult::Candidates { .. } => Decision::Candidates,
            QueryResult::None { .. } => Decision::None,
        };
        assert_eq!(
            actual, expected,
            "case {} decides differently in the calibrator and in the router",
            case.id
        );
    }
}

#[test]
fn ranking_calibration_never_answers_a_held_out_fold_wrongly() {
    let calibration = calibration();
    for fold in &calibration.folds {
        assert_eq!(
            fold.metrics.wrong_directs, 0,
            "fold {} answers a query with the wrong anchor",
            fold.held_out
        );
        assert!(
            fold.metrics.recall_ppm() >= REQUIRED_RECALL_PPM,
            "fold {} keeps only {} ppm of answerable cases in the first three results",
            fold.held_out,
            fold.metrics.recall_ppm()
        );
    }
    assert_eq!(calibration.shipped_metrics.wrong_directs, 0);
    assert!(calibration.shipped_metrics.recall_ppm() >= REQUIRED_RECALL_PPM);
}

#[test]
fn ranking_calibration_report_is_reproducible() {
    let calibration = calibration();
    print!("{}", summary(calibration));
    let text = report(calibration);
    let path = golden_path();
    if std::env::var_os("RR_UPDATE_CALIBRATION").is_some() {
        std::fs::write(&path, &text).expect("write the calibration report");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}; regenerate it with RR_UPDATE_CALIBRATION=1",
            path.display()
        )
    });
    assert_eq!(
        text, expected,
        "the calibration report moved; rerun with RR_UPDATE_CALIBRATION=1 to record it"
    );
}

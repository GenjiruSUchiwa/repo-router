//! The corpus's evidence, adjudicated rather than believed.
//!
//! A quality report is written by a corpus run and read here. It is the sole
//! integration boundary between the frozen corpus and the release decision:
//! the corpus drives the compiled `rr` as a black box and files what it
//! measured, and [`adjudicate`] decides whether that evidence blocks a release.
//! Nothing in this module runs a benchmark, opens a socket, or believes a
//! number it has not first checked.
//!
//! # A benchmark figure is never a guarantee
//!
//! A measurement is a fact about one machine, one corpus and one afternoon. A
//! published latency becomes a promise the first time somebody reads it, and
//! nothing in the workload a user brings resembles the workload it was taken
//! on. So the rule this module enforces is structural rather than editorial:
//! **no verdict in here may be spelled in milliseconds.** A performance
//! decision is a ratio against an approved baseline, and
//! [`RR0504_PERFORMANCE_REGRESSION`] is the mechanism that replaces the
//! absolute number — a regression against what this repository itself measured
//! last time, which is a claim the evidence can actually support. Any type
//! added here later must keep that property: a field holding an absolute
//! duration is how the number comes back.
//!
//! # Why the input is closed while rr's own reports are open
//!
//! [`crate::json_contract`] makes rr's `--json` *output* open, so a consumer
//! reads by key and ignores what it does not recognize. This decodes an
//! *input*, and the trade runs the other way. An unknown key, an unknown
//! schema version, an unknown rule id or a trailing byte all mean the producer
//! and this adjudicator disagree about the contract — and the one outcome a
//! release gate may never have is "the evidence was shaped oddly, so it
//! passed". Every disagreement is therefore [`RR0501_QUALITY_REPORT_INVALID`],
//! an error, and the report contributes no verdict at all.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::de::DeserializeOwned;

use crate::check::{DiagnosticV1, Severity};
use crate::path::RelPath;
use crate::ranking::{Score, MARGIN_SCALE};
use crate::text::Digest;

/// Version of the `QualityReportV1` contract a corpus run writes.
///
/// Read, never widened silently: a report declaring anything else is refused
/// under [`RR0501_QUALITY_REPORT_INVALID`] rather than decoded on the chance
/// that the fields happen to line up.
pub const QUALITY_SCHEMA_VERSION: u32 = 1;

/// The largest quality report this build will read into memory.
///
/// Checked against the file's own metadata before a single byte is allocated. A
/// report is a few hundred verdicts; anything at this scale is a mistyped path
/// pointing at an archive, and refusing it by size is what keeps a mistyped
/// flag from becoming an out-of-memory kill in CI.
pub const MAX_QUALITY_REPORT_BYTES: u64 = 16 * 1024 * 1024;

/// The report could not be adjudicated, so nothing in it is believed.
pub const RR0501_QUALITY_REPORT_INVALID: &str = "RR0501_QUALITY_REPORT_INVALID";
/// A vendored corpus repository's licence is not the one the manifest declares.
pub const RR0502_CORPUS_LICENSE_MISMATCH: &str = "RR0502_CORPUS_LICENSE_MISMATCH";
/// Routing quality fell below the gate on the frozen oracle.
pub const RR0503_ROUTING_QUALITY_REGRESSION: &str = "RR0503_ROUTING_QUALITY_REGRESSION";
/// A performance ratio regressed against the approved baseline.
pub const RR0504_PERFORMANCE_REGRESSION: &str = "RR0504_PERFORMANCE_REGRESSION";
/// A gate had no evidence to decide on, which is not the same as passing.
pub const RR0505_QUALITY_EVIDENCE_MISSING: &str = "RR0505_QUALITY_EVIDENCE_MISSING";

/// The rule ids a report is allowed to declare a finding under.
///
/// A closed list rather than a prefix test, and that is the fail-closed half of
/// this module: a report naming `RR0599` would otherwise arrive as a verdict no
/// release gate has ever agreed to, spelled by whoever wrote the report.
/// [`RR0501_QUALITY_REPORT_INVALID`] is deliberately absent — it is this
/// adjudicator's own verdict about the report, never a finding the report may
/// file about itself.
pub const ADJUDICABLE_RULES: [&str; 4] = [
    RR0502_CORPUS_LICENSE_MISMATCH,
    RR0503_ROUTING_QUALITY_REGRESSION,
    RR0504_PERFORMANCE_REGRESSION,
    RR0505_QUALITY_EVIDENCE_MISSING,
];

/// One verdict a corpus run reached, named by the rule that publishes it.
///
/// The corpus decides `blocked`; this module decides whether the decision is
/// admissible. That split is why the field is a `bool` written by the producer
/// rather than a threshold re-applied here: the thresholds live with the run
/// that measured against them, and re-deriving them from a summary would be a
/// second adjudicator that could disagree with the first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityFinding {
    /// One of [`ADJUDICABLE_RULES`].
    pub rule_id: String,
    /// Whether this finding blocks the release.
    pub blocked: bool,
    /// The one-line explanation a human reads.
    pub message: String,
    /// The gate the run was measured against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// What the run actually reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

/// One corpus run's evidence, as the file holds it.
///
/// `schema_version` is the first key, per [`crate::json_contract`], because a
/// reader must be able to refuse a document before interpreting any of it.
///
/// It serializes as well as decodes, and that direction has one purpose: the
/// corpus harness writes the report this module then reads. Two spellings of one
/// document — a writer in the harness and a reader here — is how a producer and
/// its adjudicator start disagreeing about a contract they were meant to share,
/// so [`build_report`] is the only writer and this is the only shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityReportV1 {
    /// Must equal [`QUALITY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The checksum-locked corpus manifest this evidence was produced from.
    pub manifest_digest: String,
    /// Every verdict the run reached, blocking or not.
    pub findings: Vec<QualityFinding>,
}

/// What a quality report contributed to a check, in the summary a report prints.
///
/// Counts and not the findings themselves: the findings are already in
/// [`crate::check::CheckResultV1::diagnostics`], and publishing them twice in
/// one object is how two copies of one list start to disagree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QualitySummary {
    /// The version the report declared, which is [`QUALITY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The corpus manifest the evidence came from.
    pub manifest_digest: String,
    /// Every finding the report carried.
    pub findings: u32,
    /// How many of them block.
    pub blocked: u32,
}

/// Why a quality report could not be adjudicated.
///
/// A closed set because each variant is a different thing to fix and they all
/// reach the same rule: the spelling goes in
/// [`crate::check::DiagnosticV1::actual`], so a CI log says which of the seven
/// happened without a second rule id per cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityFault {
    /// The path is not a regular file — a directory, a device, a dangling link.
    NotRegularFile,
    /// The file is larger than [`MAX_QUALITY_REPORT_BYTES`].
    TooLarge,
    /// The bytes could not be read.
    Unreadable,
    /// The bytes are not the JSON object this contract describes.
    Malformed,
    /// The object decoded and something followed it.
    TrailingBytes,
    /// `schema_version` is not [`QUALITY_SCHEMA_VERSION`].
    UnsupportedVersion,
    /// A finding names a rule outside [`ADJUDICABLE_RULES`].
    UnknownRule,
}

impl QualityFault {
    /// The published spelling, identical in text and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRegularFile => "not-a-regular-file",
            Self::TooLarge => "too-large",
            Self::Unreadable => "unreadable",
            Self::Malformed => "malformed",
            Self::TrailingBytes => "trailing-bytes",
            Self::UnsupportedVersion => "unsupported-schema-version",
            Self::UnknownRule => "unknown-rule-id",
        }
    }
}

/// What one quality report added to a check.
///
/// The summary is `None` exactly when the report was refused, so a caller
/// cannot report a corpus identity it never successfully read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjudication {
    /// The counts, when the report was admissible.
    pub summary: Option<QualitySummary>,
    /// One diagnostic per blocking finding, or the single refusal.
    pub diagnostics: Vec<DiagnosticV1>,
}

/// Reads one quality report and turns its blocking findings into diagnostics.
///
/// Read-only, like the check it feeds: nothing here writes, rewrites or removes
/// the report. The order of the questions is the order that keeps a mistyped
/// path cheap — file type, then size, then bytes, then grammar, then version,
/// then rule ids — so nothing is allocated for a path that was never a report.
///
/// A refusal is one [`RR0501_QUALITY_REPORT_INVALID`] error and no summary; a
/// report that passes contributes one error per blocking finding and nothing
/// for the rest. It never suppresses a repository rule: `--quality-report`
/// adds evidence, it does not replace the questions rr asks about the
/// repository it is standing in.
#[must_use]
pub fn adjudicate(path: &Path) -> Adjudication {
    let report = match read_report(path) {
        Ok(report) => report,
        Err(fault) => {
            return Adjudication {
                summary: None,
                diagnostics: vec![refusal(path, fault)],
            }
        }
    };

    let blocked = report.findings.iter().filter(|found| found.blocked).count();
    let diagnostics = report
        .findings
        .iter()
        .filter(|found| found.blocked)
        .map(|found| DiagnosticV1 {
            rule_id: rule_id_of(&found.rule_id),
            severity: Severity::Error,
            message: found.message.clone(),
            path: None,
            anchor: None,
            expected: found.expected.clone(),
            actual: found.actual.clone(),
            remediation: "read the corpus report, then fix the regression or \
                          re-approve the baseline it is measured against",
        })
        .collect();

    Adjudication {
        summary: Some(QualitySummary {
            schema_version: report.schema_version,
            manifest_digest: report.manifest_digest,
            findings: u32::try_from(report.findings.len()).unwrap_or(u32::MAX),
            blocked: u32::try_from(blocked).unwrap_or(u32::MAX),
        }),
        diagnostics,
    }
}

/// The `'static` spelling of a rule id the report declared.
///
/// [`ADJUDICABLE_RULES`] has already been consulted by [`read_report`], so the
/// lookup always hits; the fallback is [`RR0501_QUALITY_REPORT_INVALID`]
/// because a rule id that reached this far without being in the list would be a
/// finding filed under a name no gate agreed to, and reporting that as the
/// report being invalid is the honest reading.
fn rule_id_of(declared: &str) -> &'static str {
    ADJUDICABLE_RULES
        .into_iter()
        .find(|known| *known == declared)
        .unwrap_or(RR0501_QUALITY_REPORT_INVALID)
}

/// The refusal, as the one diagnostic a rejected report produces.
fn refusal(path: &Path, fault: QualityFault) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0501_QUALITY_REPORT_INVALID,
        severity: Severity::Error,
        message: String::from("the quality report could not be adjudicated"),
        path: Some(path.display().to_string()),
        anchor: None,
        expected: Some(format!(
            "QualityReportV1 schema_version {QUALITY_SCHEMA_VERSION}"
        )),
        actual: Some(fault.as_str().to_owned()),
        remediation: "re-run the corpus to produce a fresh report; \
                      a report rr cannot read is never treated as evidence",
    }
}

/// Reads and strictly decodes one report, or names what stopped it.
///
/// `path` is the operator's own argument, so it is not repo-relative and is not
/// meant to be: [`DiagnosticV1::path`] is repo-relative for findings *about*
/// the repository, and this one names a file outside it that the operator
/// typed.
fn read_report(path: &Path) -> Result<QualityReportV1, QualityFault> {
    let metadata = std::fs::metadata(path).map_err(|_| QualityFault::Unreadable)?;
    if !metadata.is_file() {
        return Err(QualityFault::NotRegularFile);
    }
    if metadata.len() > MAX_QUALITY_REPORT_BYTES {
        return Err(QualityFault::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| QualityFault::Unreadable)?;
    decode_report(&bytes)
}

/// Decodes the bytes of one report, rejecting anything after the object.
///
/// Split from the read so the strictness is testable without a filesystem, and
/// because "the object decoded and there was more" is the failure a plain
/// `from_slice` silently forgives.
fn decode_report(bytes: &[u8]) -> Result<QualityReportV1, QualityFault> {
    let report: QualityReportV1 = decode_strict(bytes).map_err(|fault| match fault {
        StrictFault::Malformed => QualityFault::Malformed,
        StrictFault::TrailingBytes => QualityFault::TrailingBytes,
    })?;

    if report.schema_version != QUALITY_SCHEMA_VERSION {
        return Err(QualityFault::UnsupportedVersion);
    }
    if Digest::parse(&report.manifest_digest).is_err() {
        return Err(QualityFault::Malformed);
    }
    if report
        .findings
        .iter()
        .any(|found| !ADJUDICABLE_RULES.contains(&found.rule_id.as_str()))
    {
        return Err(QualityFault::UnknownRule);
    }
    Ok(report)
}

/// One measured proportion, kept as the arithmetic that produced it.
///
/// The numerator and the denominator are published beside the ratio because a
/// bare proportion cannot be argued with: `0.900000` could be 36 of 40 or 9 of
/// 10, and only one of those clears a gate written as "36 of 40". A reviewer
/// reading a release decision has to be able to recount it.
///
/// There is no `f64` anywhere in this type. The value is computed in integers,
/// stored in integers and printed from integers, so the rendered ratio is the
/// stored ratio on every platform — a float in a committed report is a report
/// that can differ between the machine that wrote it and the machine that reads
/// it, over nothing but a rounding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Ratio {
    /// How many cases counted for.
    pub numerator: u64,
    /// How many cases were eligible to count at all.
    pub denominator: u64,
    /// `numerator / denominator` in parts per million, rounded half up.
    ///
    /// Serialized as a six-decimal string through [`Score`], the crate's owning
    /// spelling for a millionths quantity, so a ratio and a score are one
    /// format and not two.
    #[serde(serialize_with = "six_decimals")]
    pub ppm: u32,
}

impl Ratio {
    /// Builds a ratio, recording both integers and the rounded millionths.
    ///
    /// A zero denominator yields zero millionths rather than an error: "no case
    /// was eligible" is a fact a report has to be able to state, and the gate
    /// that cares — [`GateFailure::EvidenceMissing`] — asks about the
    /// denominator, not about the ratio.
    ///
    /// The millionths saturate instead of wrapping. A ratio above 4,294 is a
    /// measurement that went wrong somewhere upstream, and a wrapped value
    /// would turn a four-thousand-fold regression into a passing number.
    #[must_use]
    pub fn new(numerator: u64, denominator: u64) -> Self {
        let ppm = if denominator == 0 {
            0
        } else {
            let scale = u128::from(MARGIN_SCALE);
            let denominator = u128::from(denominator);
            let scaled = (u128::from(numerator) * scale * 2 + denominator) / (denominator * 2);
            u32::try_from(scaled).unwrap_or(u32::MAX)
        };
        Self {
            numerator,
            denominator,
            ppm,
        }
    }

    /// Whether this ratio is at least `numerator / denominator`.
    ///
    /// Cross-multiplied over the stored integers rather than compared on
    /// [`Self::ppm`]: the millionths are rounded, so a gate compared on them
    /// would pass a run that rounded up to the threshold. The comparison a
    /// release decision needs is the exact one.
    #[must_use]
    pub fn at_least(self, numerator: u64, denominator: u64) -> bool {
        u128::from(self.numerator) * u128::from(denominator)
            >= u128::from(numerator) * u128::from(self.denominator)
    }

    /// Whether this ratio is strictly greater than `numerator / denominator`.
    #[must_use]
    pub fn exceeds(self, numerator: u64, denominator: u64) -> bool {
        u128::from(self.numerator) * u128::from(denominator)
            > u128::from(numerator) * u128::from(self.denominator)
    }

    /// Whether every eligible case counted for.
    ///
    /// True of `0/0`, and deliberately: a run that answered no direct anchor
    /// produced no wrong direct anchor either. What catches a router that never
    /// commits is the top-three gate, not this one.
    #[must_use]
    pub const fn is_whole(self) -> bool {
        self.numerator == self.denominator
    }

    /// The six-decimal spelling on its own, without the arithmetic.
    #[must_use]
    pub fn to_text(self) -> String {
        Score::from_millionths(u64::from(self.ppm)).to_string()
    }
}

impl fmt::Display for Ratio {
    /// `36/40 (0.900000)` — the arithmetic first, because that is the part a
    /// reviewer recounts.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scaled = Score::from_millionths(u64::from(self.ppm));
        write!(f, "{}/{} ({scaled})", self.numerator, self.denominator)
    }
}

/// Writes a parts-per-million quantity as a six-decimal string.
///
/// Delegates to [`Score`] rather than formatting again here: the crate already
/// owns this spelling, and a second copy is how `0.30` and `0.300000` end up in
/// one artifact.
///
/// The reference is `serde`'s `serialize_with` signature, not a choice.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn six_decimals<S: serde::Serializer>(ppm: &u32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&Score::from_millionths(u64::from(*ppm)))
}

/// How many answerable questions the frozen oracle asks.
///
/// Locked, because it is the denominator every routing gate is written against:
/// a corpus that grew to 41 questions and kept the "36 of 40" gate would have
/// quietly lowered it.
pub const ORACLE_ANSWERABLE_CASES: u64 = 40;

/// The fewest must-abstain cases the oracle carries beside the answerable ones.
///
/// They never enter the answerable denominator; they exist so that a router
/// which answers everything cannot reach a passing score by guessing.
pub const ORACLE_MIN_ABSTENTION_CASES: u64 = 8;

/// The top-three recall gate, as the two integers it is written with.
pub const TOP3_GATE_NUMERATOR: u64 = 36;
/// The denominator [`TOP3_GATE_NUMERATOR`] is measured against.
pub const TOP3_GATE_DENOMINATOR: u64 = 40;

/// How many cases top-one recall may lose against the approved baseline.
///
/// Top-one is reported rather than gated, with this one exception: a slide of
/// several cases is a ranking change nobody decided, and noticing it a release
/// later is noticing it too late.
pub const TOP1_BASELINE_SLACK_CASES: u64 = 1;

/// What one routing case demanded of the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseExpectation {
    /// The oracle knows an anchor this question should reach.
    Answerable,
    /// The router must not commit: the question is ambiguous, out of
    /// vocabulary, misleadingly overlapping, or answerable only from stale
    /// bytes.
    MustAbstain,
}

/// One routing case, reduced to the five facts every metric is computed from.
///
/// Ranks and not anchors: the anchors are in the oracle file and the comparison
/// against them belongs to the harness that ran the query. What reaches the
/// metrics is the outcome of that comparison, so no metric can quietly re-decide
/// which anchor was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingCaseVerdict {
    /// What the case demanded.
    pub expectation: CaseExpectation,
    /// Whether the oracle allows a single committed anchor for this case.
    pub direct_permitted: bool,
    /// Whether the router committed to a single anchor.
    pub answered_direct: bool,
    /// One-based rank of the first accepted anchor, `None` when none appeared.
    pub accepted_rank: Option<u32>,
    /// One-based rank of the first forbidden anchor, `None` when none appeared.
    pub forbidden_rank: Option<u32>,
}

impl RoutingCaseVerdict {
    /// Whether an accepted anchor was reached inside `rank`, ahead of any
    /// forbidden one.
    ///
    /// The forbidden anchor has to be ordered against the accepted one rather
    /// than merely absent: an answer that puts a known-wrong anchor first and
    /// the right one second is not a hit at rank one, and counting it as one is
    /// how a corpus congratulates a ranker for being nearly wrong.
    #[must_use]
    pub const fn hit_at(&self, rank: u32) -> bool {
        match self.accepted_rank {
            None => false,
            Some(accepted) => {
                accepted <= rank
                    && match self.forbidden_rank {
                        None => true,
                        Some(forbidden) => forbidden > accepted,
                    }
            }
        }
    }

    /// Whether the router declined to commit, by returning candidates or none.
    #[must_use]
    pub const fn abstained(&self) -> bool {
        !self.answered_direct
    }

    /// Whether this case produced a direct anchor that should not have been one.
    ///
    /// Three separate ways to be wrong, and each alone is enough: the oracle
    /// forbade committing at all, the case was a must-abstain case, or the
    /// committed anchor was not the accepted one. A higher direct rate never
    /// excuses any of them.
    #[must_use]
    pub const fn false_direct(&self) -> bool {
        self.answered_direct
            && (!self.direct_permitted
                || matches!(self.expectation, CaseExpectation::MustAbstain)
                || !self.hit_at(1))
    }
}

/// What one corpus run measured about routing.
///
/// Every field is a [`Ratio`] or a count, so the report a release is argued from
/// carries the arithmetic and not a rendered percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct RoutingMetrics {
    /// Accepted anchor first, over the answerable cases.
    pub top1: Ratio,
    /// Accepted anchor inside the first three, over the answerable cases.
    pub top3: Ratio,
    /// Correct direct answers over all direct answers.
    pub direct_precision: Ratio,
    /// Direct answers that were wrong, in any of the three ways.
    pub false_directs: u64,
    /// Answers that declined to commit, over every routing case.
    pub abstention: Ratio,
    /// Must-abstain cases that abstained, over the must-abstain cases.
    pub required_abstention: Ratio,
}

/// Computes the routing metrics of one corpus run.
///
/// The must-abstain cases are counted in the abstention denominators and in
/// nothing else. That separation is the whole reason they can be added: folding
/// them into the answerable denominator would let eight guaranteed abstentions
/// raise a recall figure that measures something they never participated in.
#[must_use]
pub fn routing_metrics(verdicts: &[RoutingCaseVerdict]) -> RoutingMetrics {
    let answerable = count_expectation(verdicts, CaseExpectation::Answerable);
    let must_abstain = count_expectation(verdicts, CaseExpectation::MustAbstain);
    let answerable_hit = |rank: u32| {
        count(verdicts, |verdict| {
            verdict.expectation == CaseExpectation::Answerable && verdict.hit_at(rank)
        })
    };
    let directs = count(verdicts, |verdict| verdict.answered_direct);
    let false_directs = count(verdicts, RoutingCaseVerdict::false_direct);

    RoutingMetrics {
        top1: Ratio::new(answerable_hit(1), answerable),
        top3: Ratio::new(answerable_hit(3), answerable),
        direct_precision: Ratio::new(directs - false_directs, directs),
        false_directs,
        abstention: Ratio::new(
            count(verdicts, RoutingCaseVerdict::abstained),
            count(verdicts, |_| true),
        ),
        required_abstention: Ratio::new(
            count(verdicts, |verdict| {
                verdict.expectation == CaseExpectation::MustAbstain && verdict.abstained()
            }),
            must_abstain,
        ),
    }
}

/// How many verdicts came from cases carrying `expectation`.
///
/// The two populations are named here rather than matched inline at each use,
/// because they are the two denominators every routing figure is divided by. A
/// figure divided by the other one is a figure that rose without anything having
/// improved, which is the one failure this whole module is built to prevent.
fn count_expectation(verdicts: &[RoutingCaseVerdict], expectation: CaseExpectation) -> u64 {
    count(verdicts, |verdict| verdict.expectation == expectation)
}

/// How many verdicts satisfy `predicate`.
fn count(verdicts: &[RoutingCaseVerdict], predicate: impl Fn(&RoutingCaseVerdict) -> bool) -> u64 {
    let matched = verdicts.iter().filter(|verdict| predicate(verdict)).count();
    u64::try_from(matched).unwrap_or(u64::MAX)
}

/// The point estimate above which a run is slow enough to argue about: 1.10.
pub const PERF_POINT_BLOCK_NUMERATOR: u64 = 11;
/// The denominator of [`PERF_POINT_BLOCK_NUMERATOR`].
pub const PERF_POINT_BLOCK_DENOMINATOR: u64 = 10;
/// The confidence bound above which the argument is settled: 1.05.
pub const PERF_CI_BLOCK_NUMERATOR: u64 = 21;
/// The denominator of [`PERF_CI_BLOCK_NUMERATOR`].
pub const PERF_CI_BLOCK_DENOMINATOR: u64 = 20;

/// A performance verdict that cannot be spelled in milliseconds.
///
/// **There is no absolute-latency field here, and there must never be one.** A
/// duration in this struct would be read as a promise the moment somebody found
/// it — a number measured on one machine, one corpus and one afternoon,
/// reprinted as a guarantee about a workload nobody measured. The rule against
/// that is the module header's, and this type is where it is enforced
/// structurally rather than repeated: a verdict is a *ratio* against a baseline
/// this repository itself approved, so the only claim that can be made is
/// "slower than we were", which is a claim the evidence supports.
/// `perf_decision_has_no_absolute_latency_field` holds the shape.
///
/// Both bounds must be crossed to block. A point estimate alone blocks on
/// noise, and a confidence bound alone blocks on a difference too small to act
/// on; requiring both is what makes a red build worth reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PerfDecision {
    /// Measured time over baseline time, as a ratio.
    pub point_ratio: Ratio,
    /// The lower bound of the bootstrap 95% relative-change interval, as a
    /// ratio.
    pub ci_lower_ratio: Ratio,
    /// Whether this decision blocks the release.
    pub blocked: bool,
}

impl PerfDecision {
    /// Decides one benchmark case from its two ratios.
    #[must_use]
    pub fn decide(point_ratio: Ratio, ci_lower_ratio: Ratio) -> Self {
        let blocked = point_ratio.exceeds(PERF_POINT_BLOCK_NUMERATOR, PERF_POINT_BLOCK_DENOMINATOR)
            && ci_lower_ratio.exceeds(PERF_CI_BLOCK_NUMERATOR, PERF_CI_BLOCK_DENOMINATOR);
        Self {
            point_ratio,
            ci_lower_ratio,
            blocked,
        }
    }
}

/// One reason a corpus run blocks a release.
///
/// A closed set, each variant naming the rule it is published under, because a
/// release gate whose reasons were free text could not be aggregated across four
/// shards without a human reading them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateFailure {
    /// Top-three recall is below [`TOP3_GATE_NUMERATOR`] of
    /// [`TOP3_GATE_DENOMINATOR`].
    Top3BelowGate,
    /// At least one direct answer was wrong.
    FalseDirect,
    /// Direct precision is not exactly one.
    DirectPrecisionBelowOne,
    /// A must-abstain case was answered directly.
    RequiredAbstentionBelowOne,
    /// Top-one recall lost more than [`TOP1_BASELINE_SLACK_CASES`] against the
    /// approved baseline.
    Top1RegressedBeyondSlack,
    /// The oracle no longer asks the locked number of questions.
    OracleUnderSized,
    /// A golden, corruption, staleness or adversarial case did not pass.
    ///
    /// Published under [`RR0503_ROUTING_QUALITY_REGRESSION`] rather than under a
    /// rule of its own. [`ADJUDICABLE_RULES`] is closed, and a finding filed
    /// under an id no gate has agreed to adjudicate is refused as
    /// [`RR0501_QUALITY_REPORT_INVALID`] — so inventing an id here would turn a
    /// failed safety case into an unreadable report, which is strictly worse
    /// than filing it under the corpus's "the frozen evidence did not hold"
    /// rule.
    SafetyCaseFailed,
    /// A benchmark case regressed past both bounds.
    PerformanceRegression,
    /// A gate had nothing to decide on.
    ///
    /// Blocking, not passing: the one verdict a release gate may never reach is
    /// "the evidence was absent, so nothing objected".
    EvidenceMissing,
}

impl GateFailure {
    /// The published spelling, identical in text and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top3BelowGate => "top3-below-gate",
            Self::FalseDirect => "false-direct",
            Self::DirectPrecisionBelowOne => "direct-precision-below-one",
            Self::RequiredAbstentionBelowOne => "required-abstention-below-one",
            Self::Top1RegressedBeyondSlack => "top1-regressed-beyond-slack",
            Self::OracleUnderSized => "oracle-under-sized",
            Self::SafetyCaseFailed => "safety-case-failed",
            Self::PerformanceRegression => "performance-regression",
            Self::EvidenceMissing => "evidence-missing",
        }
    }

    /// The one-line explanation a human reads in the report.
    ///
    /// Written here rather than at the call site, so the four shards and the
    /// aggregate all publish one sentence per failure: a reason spelled by
    /// whichever run happened to notice it could not be compared across runs.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Top3BelowGate => "top-three recall is below the gate on the frozen oracle",
            Self::FalseDirect => "a direct answer was wrong",
            Self::DirectPrecisionBelowOne => "not every direct answer was correct",
            Self::RequiredAbstentionBelowOne => "a must-abstain case was answered directly",
            Self::Top1RegressedBeyondSlack => "top-one recall lost more than the approved slack",
            Self::OracleUnderSized => "the oracle no longer asks the locked number of questions",
            Self::SafetyCaseFailed => "a golden, corruption, staleness or adversarial case failed",
            Self::PerformanceRegression => "a benchmark case regressed past both bounds",
            Self::EvidenceMissing => "a gate had no evidence to decide on",
        }
    }

    /// The rule this failure is published under.
    #[must_use]
    pub const fn rule_id(self) -> &'static str {
        match self {
            Self::PerformanceRegression => RR0504_PERFORMANCE_REGRESSION,
            Self::EvidenceMissing => RR0505_QUALITY_EVIDENCE_MISSING,
            Self::Top3BelowGate
            | Self::FalseDirect
            | Self::DirectPrecisionBelowOne
            | Self::RequiredAbstentionBelowOne
            | Self::Top1RegressedBeyondSlack
            | Self::OracleUnderSized
            | Self::SafetyCaseFailed => RR0503_ROUTING_QUALITY_REGRESSION,
        }
    }
}

/// Everything one release decision is taken from.
///
/// Every field is an [`Option`] because the honest answer to "was this measured"
/// has three values, not two: measured and fine, measured and bad, and never
/// measured. Collapsing the third into the first is how a shard that crashed
/// before writing its evidence becomes a green release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct GateEvidence {
    /// The routing metrics of the aggregated correctness shards.
    pub routing: Option<RoutingMetrics>,
    /// Passed over total for the golden, corruption, staleness and adversarial
    /// cases.
    pub safety: Option<Ratio>,
    /// The performance decision from the one designated platform.
    pub performance: Option<PerfDecision>,
    /// Top-one recall as the approved baseline recorded it, in cases.
    pub top1_baseline: Option<u64>,
}

/// The verdict on one corpus run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GateDecision {
    /// Whether the release is blocked.
    pub blocked: bool,
    /// Every reason, sorted and deduplicated so four shards cannot produce four
    /// spellings of one failure.
    pub failures: Vec<GateFailure>,
}

/// Adjudicates one corpus run against the locked gates.
///
/// The thresholds are the issue's, unchanged: top-three at least 36 of 40, zero
/// false directs, direct precision exactly one, required-abstention accuracy
/// exactly one, every safety case passing, and a performance decision that
/// blocks only when both bounds are crossed. Top-one and the abstention rate are
/// reported, with the single exception of a slide past
/// [`TOP1_BASELINE_SLACK_CASES`].
///
/// Missing evidence blocks. Everything this function can say about a run it says
/// from integers, so two people reading the same report reach the same verdict.
#[must_use]
pub fn adjudicate_gate(evidence: &GateEvidence) -> GateDecision {
    let mut failures = Vec::new();

    match evidence.routing {
        None => failures.push(GateFailure::EvidenceMissing),
        Some(routing) => {
            if routing.top3.denominator != ORACLE_ANSWERABLE_CASES {
                failures.push(GateFailure::OracleUnderSized);
            }
            if !routing
                .top3
                .at_least(TOP3_GATE_NUMERATOR, TOP3_GATE_DENOMINATOR)
            {
                failures.push(GateFailure::Top3BelowGate);
            }
            if routing.false_directs > 0 {
                failures.push(GateFailure::FalseDirect);
            }
            if !routing.direct_precision.is_whole() {
                failures.push(GateFailure::DirectPrecisionBelowOne);
            }
            if !routing.required_abstention.is_whole() {
                failures.push(GateFailure::RequiredAbstentionBelowOne);
            }
            if let Some(baseline) = evidence.top1_baseline {
                if routing.top1.numerator + TOP1_BASELINE_SLACK_CASES < baseline {
                    failures.push(GateFailure::Top1RegressedBeyondSlack);
                }
            }
        }
    }

    match evidence.safety {
        None => failures.push(GateFailure::EvidenceMissing),
        Some(safety) => {
            if safety.denominator == 0 {
                failures.push(GateFailure::EvidenceMissing);
            } else if !safety.is_whole() {
                failures.push(GateFailure::SafetyCaseFailed);
            }
        }
    }

    match evidence.performance {
        None => failures.push(GateFailure::EvidenceMissing),
        Some(performance) => {
            if performance.blocked {
                failures.push(GateFailure::PerformanceRegression);
            }
        }
    }

    failures.sort_unstable();
    failures.dedup();
    GateDecision {
        blocked: !failures.is_empty(),
        failures,
    }
}

/// Writes one gate decision as the report `rr check --quality-report` reads.
///
/// The corpus decides, and this only spells the decision in the one shape the
/// adjudicator accepts. Every finding is blocking, because [`GateDecision`]
/// carries the reasons a release is blocked and nothing else: a passing run
/// publishes an empty `findings` list, which is a claim a reader can check
/// against [`GateDecision::blocked`] rather than a silence they have to trust.
///
/// `expected` and `actual` are filled from the evidence the decision was taken
/// from, so a red build says `36/40 (0.900000)` beside the gate it missed instead
/// of naming the rule and leaving a human to re-run the corpus to find out by how
/// much.
#[must_use]
pub fn build_report(
    manifest_digest: &str,
    evidence: &GateEvidence,
    decision: &GateDecision,
) -> QualityReportV1 {
    QualityReportV1 {
        schema_version: QUALITY_SCHEMA_VERSION,
        manifest_digest: manifest_digest.to_owned(),
        findings: decision
            .failures
            .iter()
            .map(|failure| finding(*failure, evidence))
            .collect(),
    }
}

/// One gate failure, as the finding a report files about it.
fn finding(failure: GateFailure, evidence: &GateEvidence) -> QualityFinding {
    let routing = evidence.routing;
    let (expected, actual) = match failure {
        GateFailure::Top3BelowGate => (
            Some(format!("{TOP3_GATE_NUMERATOR}/{TOP3_GATE_DENOMINATOR}")),
            routing.map(|routing| routing.top3.to_string()),
        ),
        GateFailure::FalseDirect => (
            Some(String::from("0")),
            routing.map(|routing| routing.false_directs.to_string()),
        ),
        GateFailure::DirectPrecisionBelowOne => (
            Some(String::from("1.000000")),
            routing.map(|routing| routing.direct_precision.to_string()),
        ),
        GateFailure::RequiredAbstentionBelowOne => (
            Some(String::from("1.000000")),
            routing.map(|routing| routing.required_abstention.to_string()),
        ),
        GateFailure::Top1RegressedBeyondSlack => (
            evidence
                .top1_baseline
                .map(|baseline| format!("at least {}", baseline - TOP1_BASELINE_SLACK_CASES)),
            routing.map(|routing| routing.top1.numerator.to_string()),
        ),
        GateFailure::OracleUnderSized => (
            Some(ORACLE_ANSWERABLE_CASES.to_string()),
            routing.map(|routing| routing.top3.denominator.to_string()),
        ),
        GateFailure::SafetyCaseFailed => (
            evidence
                .safety
                .map(|safety| format!("{0}/{0}", safety.denominator)),
            evidence.safety.map(|safety| safety.to_string()),
        ),
        GateFailure::PerformanceRegression => (
            Some(format!(
                "point at most {}, lower bound at most {}",
                Ratio::new(PERF_POINT_BLOCK_NUMERATOR, PERF_POINT_BLOCK_DENOMINATOR).to_text(),
                Ratio::new(PERF_CI_BLOCK_NUMERATOR, PERF_CI_BLOCK_DENOMINATOR).to_text(),
            )),
            evidence.performance.map(|performance| {
                format!(
                    "point {}, lower bound {}",
                    performance.point_ratio, performance.ci_lower_ratio
                )
            }),
        ),
        GateFailure::EvidenceMissing => (
            Some(String::from("routing, safety and performance evidence")),
            Some(absent_evidence(evidence)),
        ),
    };
    QualityFinding {
        rule_id: failure.rule_id().to_owned(),
        blocked: true,
        message: failure.summary().to_owned(),
        expected,
        actual,
    }
}

/// Which halves of the evidence were never measured, in a fixed order.
///
/// Named rather than counted: "two of three absent" tells a reader that something
/// went wrong, and "safety, performance" tells them which shard to re-run.
fn absent_evidence(evidence: &GateEvidence) -> String {
    let mut absent = Vec::new();
    if evidence.routing.is_none() {
        absent.push("routing");
    }
    if evidence.safety.is_none_or(|safety| safety.denominator == 0) {
        absent.push("safety");
    }
    if evidence.performance.is_none() {
        absent.push("performance");
    }
    if absent.is_empty() {
        return String::from("none");
    }
    absent.join(", ")
}

/// Where the frozen corpus lives, relative to the repository root.
///
/// A constant because three separate things resolve it — the verifier, the
/// harness, and the message a run prints when the corpus is not there — and a
/// corpus that had moved would otherwise be reported as a corpus that is absent.
pub const CORPUS_ROOT: &str = "fixtures/corpus";

/// The lock the corpus is verified against, relative to the repository root.
pub const CORPUS_MANIFEST_PATH: &str = "fixtures/corpus/manifest.json";

/// Version of the corpus manifest contract.
pub const CORPUS_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Version of the case-file contract the oracle and the goldens are written in.
pub const CORPUS_CASE_SCHEMA_VERSION: u32 = 1;

/// The largest manifest, oracle or case file this build will read into memory.
pub const MAX_CORPUS_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// What a corpus run says when the corpus has not been vendored.
///
/// The three upstream repositories are not in this tree, and vendoring them is a
/// human step: it needs network access and a redistribution decision, and corpus
/// tests must run offline. So the gate skips with this sentence rather than
/// failing — a red build for work nobody has been asked to do teaches a team to
/// ignore red builds — and it names the pull request that ends the skip, so the
/// skip cannot be mistaken for a passing run.
pub const CORPUS_ABSENT_SKIP: &str =
    "fixtures/corpus/manifest.json absent; see the corpus-update PR";

/// The variable that lets a run rewrite a locked artifact.
///
/// Ordinary verification can never update the corpus. Drift is a failure, and a
/// harness that repaired its own expectations would report a clean run on the
/// exact change the corpus exists to catch.
pub const ACCEPT_CORPUS_UPDATE_ENV: &str = "RR_ACCEPT_CORPUS_UPDATE";

/// The value [`ACCEPT_CORPUS_UPDATE_ENV`] must hold, exactly.
///
/// One accepted spelling, so `0`, `false` and `no` all mean "do not rewrite"
/// instead of one of them accidentally meaning the opposite.
pub const ACCEPT_CORPUS_UPDATE_VALUE: &str = "1";

/// The variable naming which shard of the corpus a run executes.
pub const CORPUS_SHARD_ENV: &str = "RR_CORPUS_SHARD";

/// How many correctness shards one corpus run is split into.
pub const CORPUS_SHARDS: u32 = 4;

/// The corpus on disk is not the corpus the manifest locks.
///
/// Deliberately absent from [`ADJUDICABLE_RULES`]: this is rr's own verdict
/// about the fixtures it is standing on, never a finding a report may file about
/// itself. A corpus run that could declare its own corpus valid would be
/// checking nothing.
pub const RR0506_CORPUS_MANIFEST_INVALID: &str = "RR0506_CORPUS_MANIFEST_INVALID";

/// The licence expressions this repository may vendor under.
///
/// A closed list and not a parser. Deciding whether an arbitrary SPDX expression
/// permits redistribution is a legal question, and a checker that answered it
/// from a grammar would be answering it wrongly and silently; a listed
/// expression is one a human has already agreed to.
pub const REDISTRIBUTABLE_SPDX: [&str; 8] = [
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MIT",
    "MIT OR Apache-2.0",
    "Unlicense OR MIT",
];

/// Why the corpus could not be trusted.
///
/// A closed set, because each variant is a different thing for a human to fix
/// and every one of them reaches the same two rules. The spelling goes in
/// [`DiagnosticV1::actual`], so a log says which of the seventeen happened
/// without a rule id per cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusFault {
    /// The manifest, or a file it declares, could not be read.
    Unreadable,
    /// The manifest is not the document this contract describes.
    ManifestMalformed,
    /// A document decoded and something followed it.
    TrailingBytes,
    /// `schema_version` is not [`CORPUS_MANIFEST_SCHEMA_VERSION`].
    UnsupportedVersion,
    /// A vendored entry's provenance is not in the published spelling.
    ProvenanceMalformed,
    /// A declared path leaves the corpus directory.
    PathEscape,
    /// A declared path does not sit under the entry that declares it.
    ForeignPath,
    /// One path is declared twice.
    DuplicateDeclaration,
    /// A declared corpus file is a symbolic link.
    SymlinkEntry,
    /// A declared corpus file is not a regular file.
    UnsupportedFileType,
    /// A declared corpus file is absent.
    MissingFile,
    /// A file under the corpus is declared nowhere.
    UndeclaredFile,
    /// A corpus file is larger than [`MAX_CORPUS_FILE_BYTES`].
    FileTooLarge,
    /// A corpus file's byte length is not the declared one.
    LengthDrift,
    /// A corpus file's mode is not the declared one.
    ModeDrift,
    /// A corpus file's bytes are not the declared ones.
    DigestDrift,
    /// The generated repository is not what this build's generator produces.
    GeneratorDrift,
    /// A vendored repository's licence is not the one the manifest declares.
    LicenseMismatch,
}

impl CorpusFault {
    /// The published spelling, identical in text and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::ManifestMalformed => "manifest-malformed",
            Self::TrailingBytes => "trailing-bytes",
            Self::UnsupportedVersion => "unsupported-schema-version",
            Self::ProvenanceMalformed => "provenance-malformed",
            Self::PathEscape => "path-escape",
            Self::ForeignPath => "path-outside-its-entry",
            Self::DuplicateDeclaration => "declared-twice",
            Self::SymlinkEntry => "symlink-entry",
            Self::UnsupportedFileType => "unsupported-file-type",
            Self::MissingFile => "missing-file",
            Self::UndeclaredFile => "undeclared-file",
            Self::FileTooLarge => "file-too-large",
            Self::LengthDrift => "byte-length-drift",
            Self::ModeDrift => "mode-drift",
            Self::DigestDrift => "digest-drift",
            Self::GeneratorDrift => "generator-drift",
            Self::LicenseMismatch => "license-mismatch",
        }
    }

    /// The one-line explanation a human reads.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Unreadable => "a corpus file could not be read",
            Self::ManifestMalformed => {
                "the corpus manifest is not the document this contract \
                                       describes"
            }
            Self::TrailingBytes => "a corpus document decoded and something followed it",
            Self::UnsupportedVersion => {
                "the corpus manifest declares an unsupported schema \
                                         version"
            }
            Self::ProvenanceMalformed => {
                "a vendored repository's provenance is not in the \
                                          published spelling"
            }
            Self::PathEscape => "a declared path leaves the corpus directory",
            Self::ForeignPath => "a declared path does not sit under the entry that declares it",
            Self::DuplicateDeclaration => "one path is declared twice",
            Self::SymlinkEntry => "a declared corpus file is a symbolic link",
            Self::UnsupportedFileType => "a declared corpus file is not a regular file",
            Self::MissingFile => "a declared corpus file is absent",
            Self::UndeclaredFile => "a file under the corpus is declared nowhere",
            Self::FileTooLarge => "a corpus file is larger than this build will read",
            Self::LengthDrift => "a corpus file's byte length is not the declared one",
            Self::ModeDrift => "a corpus file's mode is not the declared one",
            Self::DigestDrift => "a corpus file's bytes are not the declared ones",
            Self::GeneratorDrift => {
                "the generated repository is not what this build's generator \
                                     produces"
            }
            Self::LicenseMismatch => {
                "a vendored repository's licence is not the one the manifest \
                                      declares"
            }
        }
    }

    /// The rule this fault is published under.
    #[must_use]
    pub const fn rule_id(self) -> &'static str {
        match self {
            Self::LicenseMismatch => RR0502_CORPUS_LICENSE_MISMATCH,
            _ => RR0506_CORPUS_MANIFEST_INVALID,
        }
    }

    /// The edit that resolves this fault.
    #[must_use]
    pub const fn remediation(self) -> &'static str {
        match self {
            Self::LicenseMismatch => {
                "re-vendor the repository or correct its SPDX expression in a \
                 corpus-update PR; rr does not redistribute what it cannot name"
            }
            _ => {
                "open a corpus-update PR: re-vendor or re-lock the corpus, \
                 never edit a locked file in place"
            }
        }
    }
}

/// The two file modes Git distinguishes, and the only two a corpus may hold.
///
/// Normalized to Git's own pair rather than recorded as a raw permission word: a
/// checkout's umask decides the rest of the bits, and a manifest that locked them
/// would fail on a colleague's machine over nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileMode {
    /// `100644`.
    Regular,
    /// `100755`.
    Executable,
}

/// One file the manifest locks.
///
/// `path` is a `String` and not a [`RelPath`], deliberately. A path that escapes
/// the corpus has to be *reported* — with the spelling the manifest used, so the
/// line can be found and fixed — and a typed field would turn it into a decode
/// failure that names the whole document instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusFile {
    /// Corpus-relative path, normalized, `/`-separated.
    pub path: String,
    /// The mode, normalized to Git's pair.
    pub mode: FileMode,
    /// The exact byte length.
    pub bytes: u64,
    /// The digest of the bytes, in [`Digest`]'s published spelling.
    pub digest: String,
}

/// Which of the three size tiers a vendored repository fills.
///
/// `Performance` is not a fourth tier of real code: it is the generated
/// repository, counted separately so nobody reads "four repositories" as four
/// vendored ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorpusTier {
    /// The small vendored repository.
    Small,
    /// The medium vendored repository.
    Medium,
    /// The large vendored repository.
    Large,
    /// The generated repository, which is not vendored and not counted.
    Performance,
}

impl CorpusTier {
    /// The published spelling, identical in JSON and in a diagnostic.
    ///
    /// One spelling and not two: the manifest names a tier in kebab-case through
    /// serde, and a diagnostic that named it any other way would send a reader
    /// looking through the manifest for a word that is not in it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Performance => "performance",
        }
    }
}

/// Every tier a manifest may declare, which is the whole closed vocabulary.
///
/// Checks walk this list rather than the tiers a manifest happens to contain, so
/// "every tier was looked at" is a property of the code and not of the data it
/// was handed.
pub const CORPUS_TIERS: [CorpusTier; 4] = [
    CorpusTier::Small,
    CorpusTier::Medium,
    CorpusTier::Large,
    CorpusTier::Performance,
];

/// Where one corpus repository came from.
///
/// Two arms and no third: bytes are either copied from an upstream commit under a
/// licence, or produced by this build's generator from two numbers. A repository
/// that could claim neither would be a repository nobody can reproduce.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provenance {
    /// Copied from an upstream repository at an immutable commit.
    Vendored(VendoredProvenance),
    /// Produced by [`generate_synthetic`].
    Generated(GeneratedProvenance),
}

/// Where vendored bytes came from, and under what licence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendoredProvenance {
    /// The upstream repository, over `https`.
    pub upstream_url: String,
    /// The immutable commit the copy was taken at, in hexadecimal.
    pub commit: String,
    /// The retrieval date, `YYYY-MM-DD`.
    ///
    /// The one date in this contract, and it is provenance rather than output: it
    /// says when a human looked at the upstream licence. Nothing rr renders
    /// carries a timestamp.
    pub retrieved: String,
    /// The SPDX expression, from [`REDISTRIBUTABLE_SPDX`].
    pub spdx: String,
    /// The licence files copied beside the code, under `licenses/`.
    pub license_files: Vec<CorpusFile>,
}

/// The two numbers a generated repository is reproducible from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedProvenance {
    /// Must equal [`SYNTHETIC_GENERATOR_VERSION`].
    pub generator_version: u32,
    /// The seed the generator was run with.
    pub seed: u64,
    /// How many module files it produced.
    pub files: u32,
    /// [`synthetic_digest`] of that generator run.
    ///
    /// Recorded instead of ten thousand per-file checksums, and instead of ten
    /// thousand committed files: a generated repository committed beside its
    /// generator is a second copy that can silently disagree with the first.
    pub digest: String,
}

/// One repository the corpus holds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusRepository {
    /// The directory name under `repos/`, which is also the entry's identity.
    pub id: String,
    /// Which tier it fills.
    pub tier: CorpusTier,
    /// Where its bytes came from.
    pub provenance: Provenance,
    /// Every vendored file, locked. Empty for a generated repository.
    pub files: Vec<CorpusFile>,
}

/// The whole lock.
///
/// `schema_version` is the first key, per [`crate::json_contract`], so a reader
/// can refuse the document before interpreting any of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    /// Must equal [`CORPUS_MANIFEST_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The three vendored repositories and the generated one.
    pub repositories: Vec<CorpusRepository>,
    /// Everything else under the corpus: the oracle, the case files, the
    /// goldens, the adversarial fixtures and the committed baselines.
    ///
    /// Locked exactly like source: an oracle that can be edited without the lock
    /// noticing is an oracle that will be edited to match whatever the code now
    /// does.
    pub artifacts: Vec<CorpusFile>,
}

/// What one declared path is, which decides how a fault on it is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A vendored source file, or an artifact.
    Content,
    /// A copied licence file, whose every fault is a licence question first.
    License,
}

/// Verifies the frozen corpus against its own manifest.
///
/// Returns one diagnostic per fault, in a deterministic order: manifest-level
/// faults first, then declared entries in the manifest's own order, then
/// undeclared files in sorted path order. Nothing here writes, and nothing here
/// repairs — a verifier that fixed the corpus would make the next run agree with
/// whatever it had just decided.
///
/// **An absent manifest is not a fault and yields no diagnostics.** Ordinary
/// repositories have no corpus, and this one does not have it vendored yet
/// ([`CORPUS_ABSENT_SKIP`]); the caller decides whether absence means "skip" or
/// "fail", because only the caller knows whether it was promised a corpus.
///
/// The path checks are split on purpose. `..`, an absolute path and a
/// non-normalized spelling are refused by [`RelPath`], the crate's owning path
/// validator, from the string alone. Whether a path *is* a symbolic link cannot
/// be answered from a string at all — it is a question about the filesystem — so
/// it is asked with `symlink_metadata`, and the two failures are reported
/// separately because they are fixed differently.
#[must_use]
pub fn verify_manifest(root: &Path) -> Vec<DiagnosticV1> {
    let manifest_path = root.join(CORPUS_MANIFEST_PATH);
    if !manifest_path.is_file() {
        return Vec::new();
    }
    let manifest = match load_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(fault) => return vec![corpus_diagnostic(CORPUS_MANIFEST_PATH, fault, None, None)],
    };

    let corpus = root.join(CORPUS_ROOT);
    let mut diagnostics = Vec::new();
    let mut declared: BTreeMap<String, Role> = BTreeMap::new();

    for repository in &manifest.repositories {
        declare_repository(repository, &mut declared, &mut diagnostics);
    }
    for artifact in &manifest.artifacts {
        declare_file(artifact, "", Role::Content, &mut declared, &mut diagnostics);
    }
    declared.insert(String::from("manifest.json"), Role::Content);

    verify_tiers(&manifest, &mut diagnostics);
    for repository in &manifest.repositories {
        verify_repository(&corpus, repository, &mut diagnostics);
    }
    for artifact in &manifest.artifacts {
        verify_file(&corpus, artifact, Role::Content, &mut diagnostics);
    }
    verify_no_undeclared(&corpus, &declared, &mut diagnostics);
    diagnostics
}

/// Reports a tier that two repositories both claim.
///
/// The tier is the only thing in the manifest that says which of the vendored
/// trees a figure was measured on. Two entries claiming one tier therefore mean a
/// tier is *gone*, and every figure filed under the missing one was measured on a
/// tree that does not answer for it. A tier that no repository claims is not
/// reported: a corpus halfway through vendoring declares fewer than four, and a
/// verifier that failed on that would fail on every step of the human procedure
/// except the last one.
fn verify_tiers(manifest: &CorpusManifest, diagnostics: &mut Vec<DiagnosticV1>) {
    for tier in CORPUS_TIERS {
        let claimants: Vec<&str> = manifest
            .repositories
            .iter()
            .filter(|repository| repository.tier == tier)
            .map(|repository| repository.id.as_str())
            .collect();
        if claimants.len() > 1 {
            diagnostics.push(corpus_diagnostic(
                "manifest.json",
                CorpusFault::ManifestMalformed,
                Some(format!("one repository at tier {}", tier.as_str())),
                Some(claimants.join(", ")),
            ));
        }
    }
}

/// Records every path one repository entry declares.
fn declare_repository(
    repository: &CorpusRepository,
    declared: &mut BTreeMap<String, Role>,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    let prefix = format!("repos/{}/", repository.id);
    for file in &repository.files {
        declare_file(file, &prefix, Role::Content, declared, diagnostics);
    }
    match &repository.provenance {
        Provenance::Vendored(vendored) => {
            for license in &vendored.license_files {
                declare_file(license, "licenses/", Role::License, declared, diagnostics);
            }
        }
        Provenance::Generated(_) => {
            if !repository.files.is_empty() {
                diagnostics.push(corpus_diagnostic(
                    &prefix,
                    CorpusFault::ManifestMalformed,
                    Some(String::from("a generated repository declares no files")),
                    Some(format!("{} declared files", repository.files.len())),
                ));
            }
        }
    }
}

/// Records one declared path, reporting what makes it undeclarable.
fn declare_file(
    file: &CorpusFile,
    prefix: &str,
    role: Role,
    declared: &mut BTreeMap<String, Role>,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    let Ok(normalized) = RelPath::new(&file.path) else {
        diagnostics.push(corpus_diagnostic(
            &file.path,
            CorpusFault::PathEscape,
            Some(String::from("a normalized corpus-relative path")),
            None,
        ));
        return;
    };
    if normalized.as_str() != file.path {
        diagnostics.push(corpus_diagnostic(
            &file.path,
            CorpusFault::ManifestMalformed,
            Some(normalized.as_str().to_owned()),
            None,
        ));
        return;
    }
    let misplaced = if prefix.is_empty() {
        file.path.starts_with("repos/") || file.path.starts_with("licenses/")
    } else {
        !file.path.starts_with(prefix)
    };
    if misplaced {
        diagnostics.push(corpus_diagnostic(
            &file.path,
            CorpusFault::ForeignPath,
            Some(format!("a path under {prefix:?}")),
            None,
        ));
        return;
    }
    if Digest::parse(&file.digest).is_err() {
        diagnostics.push(corpus_diagnostic(
            &file.path,
            CorpusFault::ManifestMalformed,
            Some(String::from("a blake3: digest")),
            Some(file.digest.clone()),
        ));
        return;
    }
    if declared.insert(file.path.clone(), role).is_some() {
        diagnostics.push(corpus_diagnostic(
            &file.path,
            CorpusFault::DuplicateDeclaration,
            None,
            None,
        ));
    }
}

/// Verifies one repository entry's bytes and provenance.
fn verify_repository(
    corpus: &Path,
    repository: &CorpusRepository,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    match &repository.provenance {
        Provenance::Vendored(vendored) => {
            verify_provenance(&repository.id, vendored, diagnostics);
            for license in &vendored.license_files {
                verify_file(corpus, license, Role::License, diagnostics);
            }
            for file in &repository.files {
                verify_file(corpus, file, Role::Content, diagnostics);
            }
        }
        Provenance::Generated(generated) => {
            verify_generated(&repository.id, generated, diagnostics);
        }
    }
}

/// Verifies that a vendored entry says where its bytes came from.
fn verify_provenance(id: &str, vendored: &VendoredProvenance, diagnostics: &mut Vec<DiagnosticV1>) {
    let path = format!("repos/{id}");
    if !vendored.upstream_url.starts_with("https://") {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::ProvenanceMalformed,
            Some(String::from("an https:// upstream URL")),
            Some(vendored.upstream_url.clone()),
        ));
    }
    if crate::oid::Oid::from_hex(&vendored.commit).is_err() {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::ProvenanceMalformed,
            Some(String::from("a 40- or 64-digit hexadecimal commit")),
            Some(vendored.commit.clone()),
        ));
    }
    if !is_iso_date(&vendored.retrieved) {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::ProvenanceMalformed,
            Some(String::from("a YYYY-MM-DD retrieval date")),
            Some(vendored.retrieved.clone()),
        ));
    }
    if !REDISTRIBUTABLE_SPDX.contains(&vendored.spdx.as_str()) {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::LicenseMismatch,
            Some(String::from("a listed redistributable SPDX expression")),
            Some(vendored.spdx.clone()),
        ));
    }
    if vendored.license_files.is_empty() {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::LicenseMismatch,
            Some(String::from("at least one copied licence file")),
            Some(String::from("none")),
        ));
    }
}

/// Verifies that this build's generator still produces the locked repository.
fn verify_generated(
    id: &str,
    generated: &GeneratedProvenance,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    let path = format!("repos/{id}");
    if generated.generator_version != SYNTHETIC_GENERATOR_VERSION {
        diagnostics.push(corpus_diagnostic(
            &path,
            CorpusFault::GeneratorDrift,
            Some(SYNTHETIC_GENERATOR_VERSION.to_string()),
            Some(generated.generator_version.to_string()),
        ));
        return;
    }
    match synthetic_digest(generated.generator_version, generated.seed, generated.files) {
        Err(fault) => diagnostics.push(corpus_diagnostic(&path, fault, None, None)),
        Ok(digest) => {
            let observed = digest.to_text();
            if observed != generated.digest {
                diagnostics.push(corpus_diagnostic(
                    &path,
                    CorpusFault::GeneratorDrift,
                    Some(generated.digest.clone()),
                    Some(observed),
                ));
            }
        }
    }
}

/// Verifies one declared file against the bytes on disk.
fn verify_file(corpus: &Path, file: &CorpusFile, role: Role, diagnostics: &mut Vec<DiagnosticV1>) {
    let Ok(relative) = RelPath::new(&file.path) else {
        return;
    };
    let full = corpus.join(relative.as_path());
    let metadata = match std::fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(role_diagnostic(
                &file.path,
                CorpusFault::MissingFile,
                role,
                None,
                None,
            ));
            return;
        }
        Err(_) => {
            diagnostics.push(role_diagnostic(
                &file.path,
                CorpusFault::Unreadable,
                role,
                None,
                None,
            ));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::SymlinkEntry,
            role,
            None,
            None,
        ));
        return;
    }
    if !metadata.is_file() {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::UnsupportedFileType,
            role,
            None,
            None,
        ));
        return;
    }
    if metadata.len() > MAX_CORPUS_FILE_BYTES {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::FileTooLarge,
            role,
            None,
            None,
        ));
        return;
    }
    if metadata.len() != file.bytes {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::LengthDrift,
            role,
            Some(file.bytes.to_string()),
            Some(metadata.len().to_string()),
        ));
        return;
    }
    if let Some(observed) = observed_mode(&metadata) {
        if observed != file.mode {
            diagnostics.push(role_diagnostic(
                &file.path,
                CorpusFault::ModeDrift,
                role,
                Some(mode_text(file.mode).to_owned()),
                Some(mode_text(observed).to_owned()),
            ));
        }
    }
    let Ok(bytes) = std::fs::read(&full) else {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::Unreadable,
            role,
            None,
            None,
        ));
        return;
    };
    let observed = Digest::of_bytes(&bytes).to_text();
    if observed != file.digest {
        diagnostics.push(role_diagnostic(
            &file.path,
            CorpusFault::DigestDrift,
            role,
            Some(file.digest.clone()),
            Some(observed),
        ));
    }
}

/// Reports every file under the corpus that the manifest declares nowhere.
///
/// Placeholders are files like any other. Exempting a name — `.gitkeep`, a dot
/// directory, anything — is how an undeclared payload arrives under that name,
/// so the rule is that everything except the manifest itself is declared.
fn verify_no_undeclared(
    corpus: &Path,
    declared: &BTreeMap<String, Role>,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    let Ok(found) = walk_corpus(corpus) else {
        diagnostics.push(corpus_diagnostic(
            CORPUS_ROOT,
            CorpusFault::Unreadable,
            None,
            None,
        ));
        return;
    };
    for path in found {
        if !declared.contains_key(&path) {
            diagnostics.push(corpus_diagnostic(
                &path,
                CorpusFault::UndeclaredFile,
                None,
                None,
            ));
        }
    }
}

/// Every non-directory path under the corpus, corpus-relative and sorted.
///
/// Symbolic links are listed and never followed. Following one would let a link
/// inside the corpus pull a whole tree from outside it into the verification, and
/// a link that escapes is exactly the thing being looked for.
fn walk_corpus(corpus: &Path) -> std::io::Result<Vec<String>> {
    let mut found = Vec::new();
    let mut pending = vec![String::new()];
    while let Some(prefix) = pending.pop() {
        let dir = if prefix.is_empty() {
            corpus.to_path_buf()
        } else {
            corpus.join(&prefix)
        };
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let relative = match name.to_str() {
                Some(name) if prefix.is_empty() => name.to_owned(),
                Some(name) => format!("{prefix}/{name}"),
                None => {
                    found.push(format!("{prefix}/{}", name.to_string_lossy()));
                    continue;
                }
            };
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(relative);
            } else {
                found.push(relative);
            }
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// The mode of a file on disk, where the platform can answer.
///
/// `None` on a platform without Unix permission bits. A corpus that failed
/// verification on Windows over a bit Windows does not store would push every
/// contributor there to a platform-specific workaround, which is a worse outcome
/// than not checking the bit.
///
/// The `Option` is the cross-platform half of that decision and reads as
/// unnecessary from inside the Unix build alone.
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn observed_mode(metadata: &std::fs::Metadata) -> Option<FileMode> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o111 == 0 {
        Some(FileMode::Regular)
    } else {
        Some(FileMode::Executable)
    }
}

/// The mode of a file on disk, where the platform can answer.
#[cfg(not(unix))]
fn observed_mode(_metadata: &std::fs::Metadata) -> Option<FileMode> {
    None
}

/// Git's own spelling of one normalized mode.
const fn mode_text(mode: FileMode) -> &'static str {
    match mode {
        FileMode::Regular => "100644",
        FileMode::Executable => "100755",
    }
}

/// Whether `text` is a `YYYY-MM-DD` date.
///
/// Shape only, and on purpose: this is provenance a human typed, and the
/// question worth asking mechanically is whether it is a date at all. Deciding
/// whether it is *the* date the copy was taken on is not a question code can ask.
fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && [0, 1, 2, 3, 5, 6, 8, 9]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
}

/// One corpus fault, as the diagnostic a caller reports.
fn corpus_diagnostic(
    path: &str,
    fault: CorpusFault,
    expected: Option<String>,
    actual: Option<String>,
) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: fault.rule_id(),
        severity: Severity::Error,
        message: fault.summary().to_owned(),
        path: Some(format!("{CORPUS_ROOT}/{path}")),
        anchor: None,
        expected,
        actual: Some(actual.unwrap_or_else(|| fault.as_str().to_owned())),
        remediation: fault.remediation(),
    }
}

/// One corpus fault on a file whose role decides the rule it is published under.
///
/// A drifted, absent or symlinked licence file is a licence question before it is
/// a checksum question: what a release needs to know is whether rr may still
/// redistribute these bytes, and that is [`RR0502_CORPUS_LICENSE_MISMATCH`]. The
/// underlying cause is not lost — it is the `actual` field.
fn role_diagnostic(
    path: &str,
    fault: CorpusFault,
    role: Role,
    expected: Option<String>,
    actual: Option<String>,
) -> DiagnosticV1 {
    match role {
        Role::Content => corpus_diagnostic(path, fault, expected, actual),
        Role::License => DiagnosticV1 {
            expected,
            actual: Some(actual.map_or_else(
                || fault.as_str().to_owned(),
                |actual| format!("{}: {actual}", fault.as_str()),
            )),
            ..corpus_diagnostic(path, CorpusFault::LicenseMismatch, None, None)
        },
    }
}

/// Rebuilds the lock from the corpus on disk, carrying provenance forward.
///
/// This is the mechanical half of a corpus-update pull request: a human vendors
/// the code, writes down where it came from and under what licence, and this
/// recomputes every byte length, mode and digest from the tree so no lock is ever
/// typed by hand. What it deliberately cannot do is invent provenance — the
/// upstream URL, the commit, the retrieval date and the SPDX expression come from
/// `previous`, because those are the four facts a person is accountable for.
///
/// Nothing here writes. The caller decides whether to keep the result, and
/// [`ACCEPT_CORPUS_UPDATE_ENV`] is what makes that decision explicit: a harness
/// that relocked and saved in one step would repair the drift it exists to
/// report.
///
/// A path under `repos/` that belongs to no repository entry, and the manifest
/// itself, are left out of the result. They then surface as
/// [`CorpusFault::UndeclaredFile`] on the next verification, which is the honest
/// answer: a stray file under `repos/` is either a repository nobody declared or a
/// placeholder that should have been deleted.
///
/// # Errors
/// Returns the [`CorpusFault`] that stopped it, which is always about one file:
/// unreadable, a symbolic link, not a regular file, or larger than
/// [`MAX_CORPUS_FILE_BYTES`].
pub fn relock(previous: &CorpusManifest, root: &Path) -> Result<CorpusManifest, CorpusFault> {
    let corpus = root.join(CORPUS_ROOT);
    let found = walk_corpus(&corpus).map_err(|_| CorpusFault::Unreadable)?;

    let mut repositories = Vec::new();
    for repository in &previous.repositories {
        let prefix = format!("repos/{}/", repository.id);
        let (provenance, files) = match &repository.provenance {
            Provenance::Vendored(vendored) => (
                Provenance::Vendored(VendoredProvenance {
                    license_files: lock_each(
                        &corpus,
                        vendored.license_files.iter().map(|file| file.path.as_str()),
                    )?,
                    upstream_url: vendored.upstream_url.clone(),
                    commit: vendored.commit.clone(),
                    retrieved: vendored.retrieved.clone(),
                    spdx: vendored.spdx.clone(),
                }),
                lock_each(
                    &corpus,
                    found
                        .iter()
                        .filter(|path| path.starts_with(&prefix))
                        .map(String::as_str),
                )?,
            ),
            Provenance::Generated(generated) => (
                Provenance::Generated(GeneratedProvenance {
                    generator_version: SYNTHETIC_GENERATOR_VERSION,
                    seed: generated.seed,
                    files: generated.files,
                    digest: synthetic_digest(
                        SYNTHETIC_GENERATOR_VERSION,
                        generated.seed,
                        generated.files,
                    )?
                    .to_text(),
                }),
                Vec::new(),
            ),
        };
        repositories.push(CorpusRepository {
            id: repository.id.clone(),
            tier: repository.tier,
            provenance,
            files,
        });
    }

    let artifacts = lock_each(
        &corpus,
        found
            .iter()
            .filter(|path| {
                !path.starts_with("repos/")
                    && !path.starts_with("licenses/")
                    && *path != "manifest.json"
            })
            .map(String::as_str),
    )?;

    Ok(CorpusManifest {
        schema_version: CORPUS_MANIFEST_SCHEMA_VERSION,
        repositories,
        artifacts,
    })
}

/// Locks a run of paths, in the order they arrive.
fn lock_each<'paths>(
    corpus: &Path,
    paths: impl Iterator<Item = &'paths str>,
) -> Result<Vec<CorpusFile>, CorpusFault> {
    paths.map(|path| lock_file(corpus, path)).collect()
}

/// Locks one file: its normalized path, mode, byte length and digest.
fn lock_file(corpus: &Path, path: &str) -> Result<CorpusFile, CorpusFault> {
    let relative = RelPath::new(path).map_err(|_| CorpusFault::PathEscape)?;
    let full = corpus.join(relative.as_path());
    let metadata = std::fs::symlink_metadata(&full).map_err(|_| CorpusFault::Unreadable)?;
    if metadata.file_type().is_symlink() {
        return Err(CorpusFault::SymlinkEntry);
    }
    if !metadata.is_file() {
        return Err(CorpusFault::UnsupportedFileType);
    }
    if metadata.len() > MAX_CORPUS_FILE_BYTES {
        return Err(CorpusFault::FileTooLarge);
    }
    let bytes = std::fs::read(&full).map_err(|_| CorpusFault::Unreadable)?;
    Ok(CorpusFile {
        path: relative.as_str().to_owned(),
        mode: observed_mode(&metadata).unwrap_or(FileMode::Regular),
        bytes: metadata.len(),
        digest: Digest::of_bytes(&bytes).to_text(),
    })
}

/// Reads and strictly decodes one manifest.
///
/// Public because a corpus-update run has to read the manifest it is about to
/// relock, and because reading it any other way would be the second parser this
/// module exists without.
///
/// # Errors
/// Returns the [`CorpusFault`] that stopped it: unreadable, oversized, not the
/// document, followed by trailing bytes, or from a schema version this build does
/// not implement.
pub fn load_manifest(path: &Path) -> Result<CorpusManifest, CorpusFault> {
    let bytes = read_capped(path)?;
    let manifest: CorpusManifest = decode_strict(&bytes).map_err(|fault| match fault {
        StrictFault::Malformed => CorpusFault::ManifestMalformed,
        StrictFault::TrailingBytes => CorpusFault::TrailingBytes,
    })?;
    if manifest.schema_version != CORPUS_MANIFEST_SCHEMA_VERSION {
        return Err(CorpusFault::UnsupportedVersion);
    }
    Ok(manifest)
}

/// Reads one corpus file, refusing an oversized one before allocating for it.
fn read_capped(path: &Path) -> Result<Vec<u8>, CorpusFault> {
    let metadata = std::fs::metadata(path).map_err(|_| CorpusFault::Unreadable)?;
    if !metadata.is_file() {
        return Err(CorpusFault::UnsupportedFileType);
    }
    if metadata.len() > MAX_CORPUS_FILE_BYTES {
        return Err(CorpusFault::FileTooLarge);
    }
    std::fs::read(path).map_err(|_| CorpusFault::Unreadable)
}

/// What stopped a strict decode, before either caller names it in its own words.
///
/// Two causes and no more, because those are the two a lenient `from_slice`
/// forgives: bytes that are not the document, and bytes that are the document
/// followed by something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictFault {
    /// The bytes are not the document this contract describes.
    Malformed,
    /// The document decoded and something followed it.
    TrailingBytes,
}

/// Decodes one JSON document, rejecting anything after it.
///
/// One decoder for every input this module reads. A second one would eventually
/// be the lenient one, and the file it read would be the file nobody checked.
fn decode_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StrictFault> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let decoded = T::deserialize(&mut deserializer).map_err(|_| StrictFault::Malformed)?;
    deserializer
        .end()
        .map(|()| decoded)
        .map_err(|_| StrictFault::TrailingBytes)
}

/// One frozen case file: the oracle, or one family of goldens.
///
/// Generic over the case, so `queries.json`, `impact-cases.json` and
/// `check-cases.json` share one envelope, one version word and one strict
/// decoder. Three envelopes would be three chances for one of them to become the
/// lenient one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseFile<C> {
    /// Must equal [`CORPUS_CASE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The cases, in the file's own order.
    pub cases: Vec<C>,
}

/// One question the frozen oracle asks, and the answers it accepts.
///
/// `accepted` and `forbidden` are both listed. A case that only said what is
/// right could not express the interesting half of routing — that a plausible,
/// well-scoring, wrong anchor must not be returned — and it is that half the
/// abstention cases exist for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleCase {
    /// Stable identity, which is also what the shard is computed from. It never
    /// changes once published: a renamed case moves shards and silently becomes
    /// a different case.
    pub case_id: String,
    /// Which repository under `repos/` the question is asked of.
    pub repo: String,
    /// The question, exactly as it is passed to `rr query`.
    pub query: String,
    /// The `--path` narrowing, where the case uses one.
    #[serde(default)]
    pub path: Option<String>,
    /// What the case demands of the router.
    pub expectation: CaseExpectation,
    /// Whether a single committed anchor is permitted at all.
    pub direct_permitted: bool,
    /// Anchors that count as the right answer, in `rr query`'s own spelling.
    pub accepted: Vec<String>,
    /// Anchors that count as a wrong answer.
    pub forbidden: Vec<String>,
}

/// One frozen golden: an `rr` invocation, its exit code and its bytes.
///
/// One shape for impact and check goldens both. The two commands differ in what
/// they print, not in what a golden is — arguments in, exit code and bytes out —
/// and a second shape would only let the two families drift apart in how they are
/// compared.
///
/// `args` is passed to the compiled `rr` binary directly. Never to a shell, and
/// no other program is ever executed: that is what makes "the corpus runs no
/// hook, build script or binary" a property of the harness rather than a hope
/// about the fixtures.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenCase {
    /// Stable identity, which the shard is computed from.
    pub case_id: String,
    /// Which repository under `repos/` the command runs in.
    pub repo: String,
    /// The arguments after `rr`.
    pub args: Vec<String>,
    /// The exit code the run must return.
    pub exit_code: u8,
    /// Corpus-relative path of the expected bytes, itself locked in the manifest.
    pub golden: String,
}

/// Reads one frozen case file.
///
/// # Errors
/// Returns the [`CorpusFault`] that stopped it: unreadable, oversized, not the
/// document, followed by trailing bytes, or declaring a schema version this build
/// does not implement. An unknown key is a malformed document and not a tolerated
/// extension — a case file is an *input* to a release decision, and the one
/// outcome a release decision may never have is "the evidence was shaped oddly,
/// so it passed".
pub fn load_cases<C: DeserializeOwned>(path: &Path) -> Result<Vec<C>, CorpusFault> {
    let bytes = read_capped(path)?;
    let file: CaseFile<C> = decode_strict(&bytes).map_err(|fault| match fault {
        StrictFault::Malformed => CorpusFault::ManifestMalformed,
        StrictFault::TrailingBytes => CorpusFault::TrailingBytes,
    })?;
    if file.schema_version != CORPUS_CASE_SCHEMA_VERSION {
        return Err(CorpusFault::UnsupportedVersion);
    }
    Ok(file.cases)
}

/// Version of the per-shard evidence contract.
///
/// A shard file is written by one process and read by another, so it carries its
/// own version word: two shards produced by different builds are two contracts,
/// and aggregating them would average measurements nobody took together.
pub const SHARD_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Which family one corpus case belongs to.
///
/// The two are counted differently — a routing case reaches [`RoutingMetrics`], a
/// safety case reaches a pass ratio — so a case filed under the wrong family
/// would be counted in a denominator it never belonged to, and the run would
/// still look complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseKind {
    /// A question asked of the router, from the frozen oracle.
    Routing,
    /// A golden, corruption, staleness or adversarial case.
    Safety,
}

/// One case the frozen corpus declares, and the shard that must run it.
///
/// The shard is a recorded answer and not a computation this module performs. The
/// assignment is `sha256(case_id) mod` [`CORPUS_SHARDS`], and it belongs to the
/// harness that reads the case files: bringing a second hash into `rr-core` for
/// nothing but test scheduling would add a dependency to the library every user
/// links, to answer a question no user asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCase {
    /// The case's stable identity, as the case file spells it.
    pub case_id: String,
    /// Which shard is responsible for it.
    pub shard: u32,
    /// Which family it belongs to.
    pub kind: CaseKind,
}

/// One routing case's outcome, as the shard that ran it filed it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedRouting {
    /// The case this outcome belongs to.
    pub case_id: String,
    /// What the router did with it.
    pub verdict: RoutingCaseVerdict,
}

/// One safety case's outcome.
///
/// A `bool` and not a reason: what makes a golden case interesting is the bytes
/// it compared, and those are in the run's own output. What the gate needs is
/// whether it held.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedSafety {
    /// The case this outcome belongs to.
    pub case_id: String,
    /// Whether it passed.
    pub passed: bool,
}

/// Everything one shard of a corpus run measured.
///
/// `schema_version` is the first key, per [`crate::json_contract`]. `manifest_digest`
/// is carried by every shard and not written once by the aggregate, because that
/// is the only way to notice that two shards measured two different corpora — the
/// failure a re-vendored corpus produces halfway through a CI matrix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShardEvidence {
    /// Must equal [`SHARD_EVIDENCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Which shard this is, in `0..`[`CORPUS_SHARDS`].
    pub shard: u32,
    /// How many shards the run was split into.
    pub shards: u32,
    /// The corpus this shard measured, as [`Digest`] spells it.
    pub manifest_digest: String,
    /// Every routing case this shard ran.
    pub routing: Vec<ReportedRouting>,
    /// Every safety case this shard ran.
    pub safety: Vec<ReportedSafety>,
}

/// Why four shards could not be aggregated into one release decision.
///
/// A closed set, and every variant is a refusal rather than a warning. An
/// aggregate that dropped a case it could not place would compute a smaller
/// denominator and publish a *higher* score than the run earned — the one failure
/// mode a release gate cannot be allowed, because it makes an incomplete run look
/// like a better one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregationFault {
    /// No shard filed evidence for this index.
    MissingShard(u32),
    /// Two shards filed evidence for one index.
    DuplicateShard(u32),
    /// A shard names an index outside `0..`[`CORPUS_SHARDS`].
    ShardOutOfRange(u32),
    /// A shard declares a version this build does not implement.
    UnsupportedVersion(u32),
    /// A shard was split against a different number of shards.
    ShardCountMismatch {
        /// What the shard file declared.
        declared: u32,
        /// What this build splits into.
        expected: u32,
    },
    /// A shard's corpus digest is not a digest.
    DigestMalformed(String),
    /// Two shards measured different corpora.
    ManifestMismatch {
        /// The digest the first shard carried.
        first: String,
        /// The one that disagreed with it.
        other: String,
    },
    /// One case reached the aggregate twice.
    DuplicateCase(String),
    /// A shard filed a case the corpus declares nowhere.
    UnknownCase(String),
    /// A case the corpus declares reached no shard.
    MissingCase(String),
    /// A case was run by a shard it is not assigned to.
    MisshardedCase {
        /// The case.
        case_id: String,
        /// The shard that ran it.
        ran_on: u32,
        /// The shard the assignment gives it.
        belongs_to: u32,
    },
    /// A case was filed under the wrong family.
    MisfiledCase(String),
}

impl AggregationFault {
    /// The published spelling, identical in text and JSON.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MissingShard(_) => "missing-shard",
            Self::DuplicateShard(_) => "duplicate-shard",
            Self::ShardOutOfRange(_) => "shard-out-of-range",
            Self::UnsupportedVersion(_) => "unsupported-schema-version",
            Self::ShardCountMismatch { .. } => "shard-count-mismatch",
            Self::DigestMalformed(_) => "digest-malformed",
            Self::ManifestMismatch { .. } => "manifest-mismatch",
            Self::DuplicateCase(_) => "duplicate-case",
            Self::UnknownCase(_) => "unknown-case",
            Self::MissingCase(_) => "missing-case",
            Self::MisshardedCase { .. } => "missharded-case",
            Self::MisfiledCase(_) => "misfiled-case",
        }
    }
}

impl fmt::Display for AggregationFault {
    /// The spelling first, then the one fact that identifies the offender, so a
    /// CI log line is enough to find it without re-running the aggregate.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spelling = self.as_str();
        match self {
            Self::MissingShard(shard)
            | Self::DuplicateShard(shard)
            | Self::ShardOutOfRange(shard) => write!(f, "{spelling}: shard {shard}"),
            Self::UnsupportedVersion(version) => write!(f, "{spelling}: {version}"),
            Self::ShardCountMismatch { declared, expected } => {
                write!(
                    f,
                    "{spelling}: {declared} shards, this build splits into {expected}"
                )
            }
            Self::DigestMalformed(digest) | Self::ManifestMismatch { other: digest, .. } => {
                write!(f, "{spelling}: {digest}")
            }
            Self::DuplicateCase(case_id)
            | Self::UnknownCase(case_id)
            | Self::MissingCase(case_id)
            | Self::MisfiledCase(case_id) => write!(f, "{spelling}: {case_id}"),
            Self::MisshardedCase {
                case_id,
                ran_on,
                belongs_to,
            } => write!(
                f,
                "{spelling}: {case_id} ran on {ran_on}, belongs to {belongs_to}"
            ),
        }
    }
}

/// One corpus run, as the four shards together measured it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CorpusAggregate {
    /// The corpus every shard agreed it was measuring.
    pub manifest_digest: String,
    /// The routing verdicts, in the order the corpus declares its cases.
    pub routing: Vec<RoutingCaseVerdict>,
    /// Safety cases passed over safety cases run.
    pub safety: Ratio,
}

/// Aggregates every shard of one corpus run, refusing anything it cannot place.
///
/// The four questions this asks are the four ways a sharded run lies about its
/// own completeness: a case nobody ran, a case two shards ran, a case no case
/// file declares, and a case run by the wrong shard. Each of them changes a
/// denominator, and a denominator that shrank is a score that rose.
///
/// Order is the corpus's, not the shards': the routing verdicts come out in the
/// order `expected` lists them, so four shards finishing in any order produce one
/// aggregate, byte for byte.
///
/// # Errors
/// Returns the first [`AggregationFault`], which is always about one shard or one
/// case.
pub fn aggregate(
    expected: &[ExpectedCase],
    shards: &[ShardEvidence],
) -> Result<CorpusAggregate, AggregationFault> {
    let mut present: BTreeSet<u32> = BTreeSet::new();
    let mut agreed: Option<&str> = None;
    for shard in shards {
        if shard.schema_version != SHARD_EVIDENCE_SCHEMA_VERSION {
            return Err(AggregationFault::UnsupportedVersion(shard.schema_version));
        }
        if shard.shards != CORPUS_SHARDS {
            return Err(AggregationFault::ShardCountMismatch {
                declared: shard.shards,
                expected: CORPUS_SHARDS,
            });
        }
        if shard.shard >= CORPUS_SHARDS {
            return Err(AggregationFault::ShardOutOfRange(shard.shard));
        }
        if !present.insert(shard.shard) {
            return Err(AggregationFault::DuplicateShard(shard.shard));
        }
        if Digest::parse(&shard.manifest_digest).is_err() {
            return Err(AggregationFault::DigestMalformed(
                shard.manifest_digest.clone(),
            ));
        }
        match agreed {
            None => agreed = Some(&shard.manifest_digest),
            Some(first) if first != shard.manifest_digest => {
                return Err(AggregationFault::ManifestMismatch {
                    first: first.to_owned(),
                    other: shard.manifest_digest.clone(),
                })
            }
            Some(_) => {}
        }
    }
    for index in 0..CORPUS_SHARDS {
        if !present.contains(&index) {
            return Err(AggregationFault::MissingShard(index));
        }
    }

    let mut declared: BTreeMap<&str, &ExpectedCase> = BTreeMap::new();
    for case in expected {
        if declared.insert(case.case_id.as_str(), case).is_some() {
            return Err(AggregationFault::DuplicateCase(case.case_id.clone()));
        }
    }

    let mut routing: BTreeMap<&str, RoutingCaseVerdict> = BTreeMap::new();
    let mut safety: BTreeMap<&str, bool> = BTreeMap::new();
    for shard in shards {
        for reported in &shard.routing {
            let case = place(&declared, &reported.case_id, CaseKind::Routing, shard.shard)?;
            if routing.insert(&case.case_id, reported.verdict).is_some() {
                return Err(AggregationFault::DuplicateCase(case.case_id.clone()));
            }
        }
        for reported in &shard.safety {
            let case = place(&declared, &reported.case_id, CaseKind::Safety, shard.shard)?;
            if safety.insert(&case.case_id, reported.passed).is_some() {
                return Err(AggregationFault::DuplicateCase(case.case_id.clone()));
            }
        }
    }

    let mut verdicts = Vec::new();
    let mut passed = 0_u64;
    let mut safety_cases = 0_u64;
    for case in expected {
        match case.kind {
            CaseKind::Routing => {
                let verdict = routing
                    .get(case.case_id.as_str())
                    .ok_or_else(|| AggregationFault::MissingCase(case.case_id.clone()))?;
                verdicts.push(*verdict);
            }
            CaseKind::Safety => {
                let held = safety
                    .get(case.case_id.as_str())
                    .ok_or_else(|| AggregationFault::MissingCase(case.case_id.clone()))?;
                safety_cases += 1;
                if *held {
                    passed += 1;
                }
            }
        }
    }

    Ok(CorpusAggregate {
        manifest_digest: shards
            .first()
            .map_or_else(String::new, |shard| shard.manifest_digest.clone()),
        routing: verdicts,
        safety: Ratio::new(passed, safety_cases),
    })
}

/// Finds the declared case one shard filed, or names why it does not belong.
fn place<'cases>(
    declared: &BTreeMap<&str, &'cases ExpectedCase>,
    case_id: &str,
    kind: CaseKind,
    ran_on: u32,
) -> Result<&'cases ExpectedCase, AggregationFault> {
    let case = declared
        .get(case_id)
        .ok_or_else(|| AggregationFault::UnknownCase(case_id.to_owned()))?;
    if case.kind != kind {
        return Err(AggregationFault::MisfiledCase(case_id.to_owned()));
    }
    if case.shard != ran_on {
        return Err(AggregationFault::MisshardedCase {
            case_id: case_id.to_owned(),
            ran_on,
            belongs_to: case.shard,
        });
    }
    Ok(case)
}

/// Version of the performance-evidence contract a benchmark run writes.
pub const PERF_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// One measured proportion, as the two integers a benchmark run has.
///
/// Deliberately not a [`Ratio`]: a `Ratio` publishes its rounded millionths as a
/// six-decimal string, which is an output spelling. What crosses the file
/// boundary is the arithmetic, and the millionths are then derived here — so the
/// evidence a decision is taken from cannot have been rounded by whoever wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatioPair {
    /// The measured quantity.
    pub numerator: u64,
    /// What it is measured against.
    pub denominator: u64,
}

impl RatioPair {
    /// The ratio these two integers describe.
    #[must_use]
    pub fn to_ratio(self) -> Ratio {
        Ratio::new(self.numerator, self.denominator)
    }
}

/// What one benchmark run concluded, on the one designated platform.
///
/// **Two ratios and no duration**, exactly as [`PerfDecision`] holds none: the
/// module header's rule applies to what crosses a file boundary as much as to
/// what a type holds, and a milliseconds field here would be the number coming
/// back the long way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfEvidenceV1 {
    /// Must equal [`PERF_EVIDENCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Measured over baseline, as the point estimate.
    pub point: RatioPair,
    /// The lower bound of the bootstrap 95% relative-change interval.
    pub ci_lower: RatioPair,
}

impl PerfEvidenceV1 {
    /// The verdict these two ratios reach.
    #[must_use]
    pub fn decide(self) -> PerfDecision {
        PerfDecision::decide(self.point.to_ratio(), self.ci_lower.to_ratio())
    }
}

/// Reads the performance evidence one benchmark run filed.
///
/// Separate from the shard files because it comes from somewhere else: the
/// correctness shards run anywhere, and this is written by the one pinned
/// platform. Absent is not zero — the caller leaves
/// [`GateEvidence::performance`] `None`, which blocks under
/// [`GateFailure::EvidenceMissing`], because a release that did not measure
/// performance has not shown that performance held.
///
/// # Errors
/// Returns the [`CorpusFault`] that stopped it: unreadable, oversized, not the
/// document, followed by trailing bytes, or from a schema version this build does
/// not implement.
pub fn load_perf_evidence(path: &Path) -> Result<PerfEvidenceV1, CorpusFault> {
    let bytes = read_capped(path)?;
    let evidence: PerfEvidenceV1 = decode_strict(&bytes).map_err(|fault| match fault {
        StrictFault::Malformed => CorpusFault::ManifestMalformed,
        StrictFault::TrailingBytes => CorpusFault::TrailingBytes,
    })?;
    if evidence.schema_version != PERF_EVIDENCE_SCHEMA_VERSION {
        return Err(CorpusFault::UnsupportedVersion);
    }
    Ok(evidence)
}

/// Version of the generated repository's shape.
///
/// Bumped whenever the generator's output changes by a byte, so a manifest locked
/// against the old shape is refused as [`CorpusFault::GeneratorDrift`] rather
/// than compared against bytes it never saw.
pub const SYNTHETIC_GENERATOR_VERSION: u32 = 1;

/// How many module files the performance repository holds.
pub const SYNTHETIC_FILE_COUNT: u32 = 10_000;

/// The directory the generated repository is declared under.
pub const SYNTHETIC_REPO_ID: &str = "synthetic-10k";

/// One generated file, before anything has written it.
///
/// `path` is a `String` rather than a [`RelPath`] because the place a path has to
/// be proven relative is the place that joins it to a directory —
/// [`materialize_synthetic`] — and validating it here instead would move the check
/// away from the write it protects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticFile {
    /// Repository-relative path, `/`-separated.
    pub path: String,
    /// The whole file.
    pub contents: String,
}

/// Generates the performance repository from two numbers.
///
/// Deterministic by construction: every byte comes from `seed` and the file's
/// index through integer mixing, under a `version` that is checked rather than
/// mixed in, so two machines two years apart produce the same ten thousand files.
/// There is no clock, no path from the host, no hash-map traversal and no
/// floating point anywhere in the output.
///
/// The repository is generated and never committed. Ten thousand committed files
/// beside the generator that claims to produce them are a second copy that can
/// disagree with the first, and the manifest locks the
/// [`synthetic_digest`] instead precisely so that disagreement is what fails.
///
/// # Errors
/// Returns [`CorpusFault::GeneratorDrift`] when `version` is not
/// [`SYNTHETIC_GENERATOR_VERSION`]: this build can only produce its own shape,
/// and quietly producing a different one would make a locked digest unmeetable.
pub fn generate_synthetic(
    version: u32,
    seed: u64,
    files: u32,
) -> Result<Vec<SyntheticFile>, CorpusFault> {
    if version != SYNTHETIC_GENERATOR_VERSION {
        return Err(CorpusFault::GeneratorDrift);
    }
    let mut generated = Vec::new();
    generated.push(SyntheticFile {
        path: String::from("Cargo.toml"),
        contents: format!(
            "[package]\nname = \"{SYNTHETIC_REPO_ID}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n"
        ),
    });

    let mut root = String::new();
    for index in 0..files {
        let module = module_name(index);
        root.push_str("pub mod ");
        root.push_str(&module);
        root.push_str(";\n");
    }
    generated.push(SyntheticFile {
        path: String::from("src/lib.rs"),
        contents: root,
    });

    for index in 0..files {
        generated.push(synthetic_module(seed, index, files));
    }
    Ok(generated)
}

/// One generated module, with a call into another one so the index has edges.
fn synthetic_module(seed: u64, index: u32, files: u32) -> SyntheticFile {
    let base = mix(seed ^ (u64::from(index) << 1));
    let first = mix(base);
    let second = mix(first);
    let callee = if files == 0 {
        index
    } else {
        u32::try_from(mix(second) % u64::from(files)).unwrap_or(index)
    };
    let name = module_name(index);
    let callee_name = module_name(callee);
    let contents = format!(
        "pub fn {name}_scramble(input: u64) -> u64 {{\n    \
         input.wrapping_mul({first}).rotate_left({rotation}) ^ {second}\n}}\n\n\
         pub fn {name}_forward(input: u64) -> u64 {{\n    \
         crate::{callee_name}::{callee_name}_scramble(input).wrapping_add({base})\n}}\n\n\
         pub struct {name}_State {{\n    pub seen: u64,\n}}\n\n\
         impl {name}_State {{\n    \
         pub fn advance(&mut self, input: u64) -> u64 {{\n        \
         self.seen = {name}_scramble(self.seen ^ input);\n        self.seen\n    }}\n}}\n",
        rotation = base % 64,
    );
    SyntheticFile {
        path: format!("src/{name}.rs"),
        contents,
    }
}

/// The module name of one generated index, zero-padded so sort order is index
/// order.
fn module_name(index: u32) -> String {
    format!("m{index:05}")
}

/// The identity of one generator run.
///
/// A length-delimited stream, so no arrangement of paths and contents can produce
/// the byte sequence of a different arrangement — a file named `ab` holding `c`
/// and a file named `a` holding `bc` would otherwise lock to the same digest.
///
/// # Errors
/// Returns whatever [`generate_synthetic`] refused.
pub fn synthetic_digest(version: u32, seed: u64, files: u32) -> Result<Digest, CorpusFault> {
    let generated = generate_synthetic(version, seed, files)?;
    let mut stream: Vec<u8> = Vec::new();
    stream.extend_from_slice(b"rr-corpus-synthetic\0");
    stream.extend_from_slice(&version.to_le_bytes());
    stream.extend_from_slice(&seed.to_le_bytes());
    stream.extend_from_slice(&files.to_le_bytes());
    for file in &generated {
        write_delimited(&mut stream, file.path.as_bytes());
        write_delimited(&mut stream, file.contents.as_bytes());
    }
    Ok(Digest::of_bytes(&stream))
}

/// Appends one length-prefixed field to a hash stream.
fn write_delimited(stream: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    stream.extend_from_slice(&length.to_le_bytes());
    stream.extend_from_slice(bytes);
}

/// Writes the generated repository into a directory the caller owns.
///
/// `dir` is a scratch directory — a temporary one, or something under `target/`.
/// The corpus never holds these files, so nothing here writes into the repository
/// being tested.
///
/// Every generated path is proven relative through [`RelPath`] immediately before
/// it is joined. That is the one place the check matters: a generator changed to
/// emit `../../etc/passwd` would otherwise be a generator that writes there.
///
/// # Errors
/// Returns the first I/O error, or [`std::io::ErrorKind::InvalidInput`] if a
/// generated path is not relative.
pub fn materialize_synthetic(
    dir: &Path,
    version: u32,
    seed: u64,
    files: u32,
) -> std::io::Result<()> {
    let generated = generate_synthetic(version, seed, files)
        .map_err(|fault| std::io::Error::other(fault.summary()))?;
    for file in &generated {
        let relative = RelPath::new(&file.path).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let full = dir.join(relative.as_path());
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, &file.contents)?;
    }
    Ok(())
}

/// The `splitmix64` finalizer, as the whole of this corpus's randomness.
///
/// Integer mixing rather than a hash function or a random-number crate: the
/// corpus has to be reproducible from two numbers on every platform this builds
/// on, and the guarantee that a hash implementation's output never changes is one
/// this repository would be relying on rather than making.
const fn mix(state: u64) -> u64 {
    let mut word = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    word = (word ^ (word >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    word = (word ^ (word >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    word ^ (word >> 31)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The `blake3:` prefix is part of the spelling `Digest::parse` accepts, and
    /// a fixture without it would make every test here pass on the digest check
    /// failing rather than on the thing it names.
    const DIGEST: &str = "blake3:0000000000000000000000000000000000000000000000000000000000000000";

    fn report(findings: &str) -> String {
        format!(
            "{{\"schema_version\":1,\"manifest_digest\":\"{DIGEST}\",\"findings\":[{findings}]}}"
        )
    }

    #[test]
    fn a_wellformed_report_contributes_only_its_blocking_findings() {
        let bytes = report(
            "{\"rule_id\":\"RR0504_PERFORMANCE_REGRESSION\",\"blocked\":true,\
             \"message\":\"cold map regressed\",\"expected\":\"1.050000\",\"actual\":\"1.310000\"},\
             {\"rule_id\":\"RR0503_ROUTING_QUALITY_REGRESSION\",\"blocked\":false,\
             \"message\":\"routing held\"}",
        );
        let decoded = decode_report(bytes.as_bytes()).unwrap();

        assert_eq!(decoded.findings.len(), 2);
        assert_eq!(decoded.manifest_digest, DIGEST);
    }

    /// The failure a plain `from_slice` forgives, and the one the rejection
    /// checklist names: a valid prefix followed by anything at all.
    #[test]
    fn trailing_bytes_after_the_object_are_refused() {
        let bytes = format!("{} garbage", report(""));

        assert_eq!(
            decode_report(bytes.as_bytes()),
            Err(QualityFault::TrailingBytes)
        );
    }

    #[test]
    fn an_unknown_schema_version_fails_closed() {
        let bytes = report("").replace("\"schema_version\":1", "\"schema_version\":2");

        assert_eq!(
            decode_report(bytes.as_bytes()),
            Err(QualityFault::UnsupportedVersion)
        );
    }

    #[test]
    fn an_unknown_rule_id_fails_closed() {
        let bytes =
            report("{\"rule_id\":\"RR0599_INVENTED\",\"blocked\":true,\"message\":\"whatever\"}");

        assert_eq!(
            decode_report(bytes.as_bytes()),
            Err(QualityFault::UnknownRule)
        );
    }

    /// An added key is a producer this adjudicator has never agreed with, and a
    /// gate that shrugged at it would pass evidence nobody wrote for it.
    #[test]
    fn an_unknown_key_fails_closed() {
        let bytes = report("").replace("\"findings\"", "\"surprise\":1,\"findings\"");

        assert_eq!(
            decode_report(bytes.as_bytes()),
            Err(QualityFault::Malformed)
        );
    }

    #[test]
    fn a_manifest_digest_that_is_not_a_digest_fails_closed() {
        let bytes = report("").replace(DIGEST, "not-a-digest");

        assert_eq!(
            decode_report(bytes.as_bytes()),
            Err(QualityFault::Malformed)
        );
    }

    /// Pins the module doc's structural rule: every fault has a spelling, and
    /// none of them is a duration.
    #[test]
    fn every_fault_spelling_is_distinct_and_kebab_case() {
        let faults = [
            QualityFault::NotRegularFile,
            QualityFault::TooLarge,
            QualityFault::Unreadable,
            QualityFault::Malformed,
            QualityFault::TrailingBytes,
            QualityFault::UnsupportedVersion,
            QualityFault::UnknownRule,
        ];
        let mut spellings: Vec<&str> = faults.iter().map(|fault| fault.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();

        assert_eq!(spellings.len(), count, "two faults share one spelling");
        for spelling in spellings {
            assert!(
                spelling
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{spelling} is not kebab-case"
            );
        }
    }

    #[test]
    fn a_refused_report_carries_no_summary() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("quality.json");
        std::fs::write(&path, b"{").unwrap();

        let adjudication = adjudicate(&path);

        assert!(adjudication.summary.is_none());
        assert_eq!(adjudication.diagnostics.len(), 1);
        assert_eq!(
            adjudication.diagnostics[0].rule_id,
            RR0501_QUALITY_REPORT_INVALID
        );
        assert_eq!(adjudication.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn a_directory_is_refused_before_any_read() {
        let temp = tempfile::tempdir().unwrap();

        let adjudication = adjudicate(temp.path());

        assert_eq!(
            adjudication.diagnostics[0].actual.as_deref(),
            Some(QualityFault::NotRegularFile.as_str())
        );
    }

    /// Every corpus fault, so the list below is the enum and not a subset of it.
    const CORPUS_FAULTS: [CorpusFault; 18] = [
        CorpusFault::Unreadable,
        CorpusFault::ManifestMalformed,
        CorpusFault::TrailingBytes,
        CorpusFault::UnsupportedVersion,
        CorpusFault::ProvenanceMalformed,
        CorpusFault::PathEscape,
        CorpusFault::ForeignPath,
        CorpusFault::DuplicateDeclaration,
        CorpusFault::SymlinkEntry,
        CorpusFault::UnsupportedFileType,
        CorpusFault::MissingFile,
        CorpusFault::UndeclaredFile,
        CorpusFault::FileTooLarge,
        CorpusFault::LengthDrift,
        CorpusFault::ModeDrift,
        CorpusFault::DigestDrift,
        CorpusFault::GeneratorDrift,
        CorpusFault::LicenseMismatch,
    ];

    #[test]
    fn every_corpus_fault_spelling_is_distinct_and_kebab_case() {
        let mut spellings: Vec<&str> = CORPUS_FAULTS.iter().map(|fault| fault.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();

        assert_eq!(spellings.len(), count, "two faults share one spelling");
        for spelling in spellings {
            assert!(
                spelling
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                "{spelling} is not kebab-case"
            );
        }
    }

    /// Exactly one fault is a licence question, and every other one is the
    /// manifest's. A fault filed under the wrong rule is a CI job paging the
    /// wrong person.
    #[test]
    fn only_a_licence_fault_is_published_under_rr0502() {
        for fault in CORPUS_FAULTS {
            let expected = if fault == CorpusFault::LicenseMismatch {
                RR0502_CORPUS_LICENSE_MISMATCH
            } else {
                RR0506_CORPUS_MANIFEST_INVALID
            };
            assert_eq!(fault.rule_id(), expected, "{}", fault.as_str());
        }
    }

    /// The manifest rule is rr's own verdict about its fixtures, so a report may
    /// not file it about itself.
    #[test]
    fn the_manifest_rule_is_not_adjudicable() {
        assert!(!ADJUDICABLE_RULES.contains(&RR0506_CORPUS_MANIFEST_INVALID));
    }

    #[test]
    fn a_ratio_publishes_its_arithmetic_beside_six_decimals() {
        let ratio = Ratio::new(36, 40);

        assert_eq!(ratio.numerator, 36);
        assert_eq!(ratio.denominator, 40);
        assert_eq!(ratio.to_text(), "0.900000");
        assert_eq!(
            serde_json::to_string(&ratio).unwrap(),
            "{\"numerator\":36,\"denominator\":40,\"ppm\":\"0.900000\"}"
        );
        assert_eq!(ratio.to_string(), "36/40 (0.900000)");
    }

    /// The millionths are rounded, so a gate compared on them would pass a run
    /// that rounded up to the threshold. 35 of 40 rounds to `0.875000` and is
    /// below the gate; 36 of 40 is exactly it.
    #[test]
    fn a_gate_is_compared_on_the_integers_not_the_millionths() {
        assert!(!Ratio::new(35, 40).at_least(TOP3_GATE_NUMERATOR, TOP3_GATE_DENOMINATOR));
        assert!(Ratio::new(36, 40).at_least(TOP3_GATE_NUMERATOR, TOP3_GATE_DENOMINATOR));
        assert!(Ratio::new(9, 10).at_least(TOP3_GATE_NUMERATOR, TOP3_GATE_DENOMINATOR));
        assert!(!Ratio::new(9, 10).exceeds(TOP3_GATE_NUMERATOR, TOP3_GATE_DENOMINATOR));
    }

    /// A zero denominator is a fact a report has to be able to state, and it must
    /// not be a division.
    #[test]
    fn an_empty_denominator_is_zero_millionths_and_whole() {
        let ratio = Ratio::new(0, 0);

        assert_eq!(ratio.ppm, 0);
        assert!(ratio.is_whole());
    }

    /// Pins the module header's structural rule and D10: the verdict is a ratio
    /// against a baseline, and there is nowhere in it to write a duration. A
    /// field holding one is how the published benchmark number comes back as a
    /// guarantee.
    #[test]
    fn perf_decision_has_no_absolute_latency_field() {
        let decision = PerfDecision::decide(Ratio::new(11, 10), Ratio::new(21, 20));
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&decision).unwrap()).unwrap();

        let mut keys = Vec::new();
        collect_keys(&json, &mut keys);
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "blocked",
                "ci_lower_ratio",
                "denominator",
                "denominator",
                "numerator",
                "numerator",
                "point_ratio",
                "ppm",
                "ppm",
            ]
        );
        for forbidden in [
            "ms", "millis", "nanos", "micros", "seconds", "duration", "latency", "elapsed", "time",
        ] {
            assert!(
                keys.iter().all(|key| !key.contains(forbidden)),
                "PerfDecision grew a {forbidden} field; a verdict in absolute time is a guarantee"
            );
        }
    }

    /// Every key in a serialized value, nested keys included.
    fn collect_keys(value: &serde_json::Value, into: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, nested) in map {
                    into.push(key.clone());
                    collect_keys(nested, into);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_keys(item, into);
                }
            }
            _ => {}
        }
    }

    /// Both bounds, or neither: a point estimate alone blocks on noise and a
    /// confidence bound alone blocks on a difference too small to act on.
    #[test]
    fn a_performance_decision_needs_both_bounds() {
        assert!(!PerfDecision::decide(Ratio::new(130, 100), Ratio::new(102, 100)).blocked);
        assert!(!PerfDecision::decide(Ratio::new(106, 100), Ratio::new(106, 100)).blocked);
        assert!(PerfDecision::decide(Ratio::new(131, 100), Ratio::new(106, 100)).blocked);
    }

    /// A known-wrong anchor ahead of the right one is not a hit at rank one, and
    /// counting it as one congratulates a ranker for being nearly wrong.
    #[test]
    fn a_forbidden_anchor_ahead_of_the_accepted_one_is_not_a_hit() {
        let poisoned = RoutingCaseVerdict {
            expectation: CaseExpectation::Answerable,
            direct_permitted: true,
            answered_direct: false,
            accepted_rank: Some(2),
            forbidden_rank: Some(1),
        };

        assert!(!poisoned.hit_at(3));
        assert!(RoutingCaseVerdict {
            forbidden_rank: Some(3),
            ..poisoned
        }
        .hit_at(3));
    }

    #[test]
    fn every_gate_failure_names_a_corpus_rule() {
        for failure in [
            GateFailure::Top3BelowGate,
            GateFailure::FalseDirect,
            GateFailure::DirectPrecisionBelowOne,
            GateFailure::RequiredAbstentionBelowOne,
            GateFailure::Top1RegressedBeyondSlack,
            GateFailure::OracleUnderSized,
            GateFailure::SafetyCaseFailed,
            GateFailure::PerformanceRegression,
            GateFailure::EvidenceMissing,
        ] {
            assert!(
                ADJUDICABLE_RULES.contains(&failure.rule_id()),
                "{} is filed under a rule no gate adjudicates",
                failure.as_str()
            );
        }
    }

    /// The one verdict a release gate may never reach is "the evidence was
    /// absent, so nothing objected".
    #[test]
    fn missing_evidence_blocks_rather_than_passes() {
        let decision = adjudicate_gate(&GateEvidence {
            routing: None,
            safety: None,
            performance: None,
            top1_baseline: None,
        });

        assert!(decision.blocked);
        assert_eq!(decision.failures, vec![GateFailure::EvidenceMissing]);
    }

    /// An added key in a case file is a producer nobody agreed with, and a gate
    /// that shrugged at it would run cases nobody wrote.
    #[test]
    fn a_case_file_rejects_an_unknown_key() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queries.json");
        std::fs::write(
            &path,
            b"{\"schema_version\":1,\"cases\":[],\"surprise\":true}",
        )
        .unwrap();

        assert_eq!(
            load_cases::<OracleCase>(&path),
            Err(CorpusFault::ManifestMalformed)
        );
    }

    #[test]
    fn a_case_file_from_another_schema_version_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("queries.json");
        std::fs::write(&path, b"{\"schema_version\":2,\"cases\":[]}").unwrap();

        assert_eq!(
            load_cases::<OracleCase>(&path),
            Err(CorpusFault::UnsupportedVersion)
        );
    }

    #[test]
    fn a_case_file_with_trailing_bytes_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("impact-cases.json");
        std::fs::write(&path, b"{\"schema_version\":1,\"cases\":[]} garbage").unwrap();

        assert_eq!(
            load_cases::<GoldenCase>(&path),
            Err(CorpusFault::TrailingBytes)
        );
    }

    /// The generator's contract is that two numbers reproduce the corpus. A build
    /// that cannot produce the locked shape says so instead of producing another.
    #[test]
    fn a_generator_version_this_build_does_not_implement_is_refused() {
        assert_eq!(
            generate_synthetic(SYNTHETIC_GENERATOR_VERSION + 1, 7, 4),
            Err(CorpusFault::GeneratorDrift)
        );
    }

    /// The paths a write is bounded by, checked where the write happens.
    #[test]
    fn every_generated_path_is_relative_and_sorted_by_index() {
        let generated = generate_synthetic(SYNTHETIC_GENERATOR_VERSION, 42, 12).unwrap();

        assert_eq!(generated.len(), 14);
        for file in &generated {
            let relative = RelPath::new(&file.path).unwrap();
            assert_eq!(relative.as_str(), file.path);
        }
        assert_eq!(generated[2].path, "src/m00000.rs");
        assert_eq!(generated[13].path, "src/m00011.rs");
    }

    /// A file named `ab` holding `c` and a file named `a` holding `bc` must not
    /// lock to one digest, which is what the length prefixes are for.
    #[test]
    fn the_synthetic_digest_separates_paths_from_contents() {
        let one = synthetic_digest(SYNTHETIC_GENERATOR_VERSION, 1, 4).unwrap();
        let other = synthetic_digest(SYNTHETIC_GENERATOR_VERSION, 1, 5).unwrap();

        assert_ne!(one, other);
        assert_eq!(
            one,
            synthetic_digest(SYNTHETIC_GENERATOR_VERSION, 1, 4).unwrap()
        );
    }
}

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

use std::path::Path;

use serde::Deserialize as _;

use crate::check::{DiagnosticV1, Severity};
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityFinding {
    /// One of [`ADJUDICABLE_RULES`].
    pub rule_id: String,
    /// Whether this finding blocks the release.
    pub blocked: bool,
    /// The one-line explanation a human reads.
    pub message: String,
    /// The gate the run was measured against.
    #[serde(default)]
    pub expected: Option<String>,
    /// What the run actually reached.
    #[serde(default)]
    pub actual: Option<String>,
}

/// One corpus run's evidence, as the file holds it.
///
/// `schema_version` is the first key, per [`crate::json_contract`], because a
/// reader must be able to refuse a document before interpreting any of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
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
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let report = QualityReportV1::deserialize(&mut deserializer)
        .map_err(|_| QualityFault::Malformed)
        .and_then(|report| {
            deserializer
                .end()
                .map(|()| report)
                .map_err(|_| QualityFault::TrailingBytes)
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
}

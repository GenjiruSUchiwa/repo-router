//! What a refresh or status run reports, and the two ways it is spelled.
//!
//! Text and JSON are two renderings of one value. Neither renderer recomputes a
//! counter or re-derives freshness: an agent parsing JSON and a human reading
//! the summary must never be able to disagree about what happened.

use crate::oid::Oid;

use super::{FullReason, RefreshOutcome};

/// Version of the `refresh`/`status` JSON contract.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Which command produced a report.
///
/// `rr map` is `rr refresh --full` with a different name, so it shares the
/// report and differs only in how the report announces itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshCommand {
    /// `rr refresh`.
    Refresh,
    /// `rr map`.
    Map,
}

impl RefreshCommand {
    /// The published command name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Map => "map",
        }
    }
}

/// The published spelling of how much work the refresh was allowed to skip.
///
/// This is finer than the requested mode: a caller that asked for `incremental`
/// and got a full rebuild must be able to see that from the report alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportedMode {
    /// A delta was planned and honoured.
    #[default]
    Incremental,
    /// A full rebuild was requested outright.
    Full,
    /// A delta was requested but could not be trusted.
    FallbackFull,
}

impl ReportedMode {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incremental => "incremental",
            Self::Full => "full",
            Self::FallbackFull => "fallback-full",
        }
    }
}

/// Everything one refresh did, and the evidence for which mechanism ran.
///
/// The counters are not decoration. "Nothing changed" and "everything was
/// re-read and happened to produce the same bytes" have the same output and
/// wildly different cost, and only these counters tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct RefreshReport {
    /// Whether a new snapshot was published.
    pub outcome: RefreshOutcome,
    /// How much of the repository was rebuilt.
    pub mode: ReportedMode,
    /// Why a delta was abandoned, when one was requested.
    pub fallback_reason: Option<FullReason>,
    /// Paths the Git delta reported; always `0` for a full rebuild, which
    /// consults no delta.
    pub changed: u64,
    /// Files handed to the extractor.
    pub reparsed: u64,
    /// Files whose facts came from the on-disk fact cache.
    pub cached: u64,
    /// Cache entries that existed but could not be decoded.
    pub cache_corrupt: u64,
    /// Canonical content acquisitions, including pre-commit re-verification.
    pub content_reads: u64,
    /// Paths in the previous snapshot that the new one does not contain.
    pub removed: u64,
    /// Rename pairs the delta reported.
    pub renamed: u64,
    /// Files whose extraction produced degraded facts.
    pub degraded: u64,
    /// Paths with unmerged index stages.
    pub conflicted: u64,
    /// Whether the snapshot file was replaced.
    pub snapshot_updated: bool,
    /// Wall-clock duration of the run.
    pub elapsed_ms: u64,
}

impl RefreshOutcome {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
        }
    }
}

/// Renders one refresh report as the single-line human summary.
///
/// Counters that are zero and had nothing to say are omitted, so the line
/// stays about what happened rather than about what did not.
#[must_use]
pub fn render_refresh_text(report: &RefreshReport, command: RefreshCommand) -> String {
    let fallback = report.fallback_reason.map_or_else(String::new, |reason| {
        format!(" (full fallback: {})", reason.as_text())
    });

    // `reparsed` is always present: it is the one counter whose absence would
    // be read as "the number is unknown" rather than "the number is zero".
    let mut counters = vec![format!("{} reparsed", report.reparsed)];
    match report.outcome {
        // Nothing was published, so the interesting question is what it cost to
        // find that out — which is content reads, not cache hits.
        RefreshOutcome::Unchanged => {
            counters.push(format!("{} content reads", report.content_reads));
        }
        RefreshOutcome::Updated => {
            counters.push(format!("{} cached", report.cached));
            counters.extend(
                [
                    (report.removed, "removed"),
                    (report.renamed, "renamed"),
                    (report.degraded, "degraded"),
                    (report.conflicted, "conflicted"),
                    (report.cache_corrupt, "cache corrupt"),
                ]
                .into_iter()
                .filter(|&(count, _)| count > 0)
                .map(|(count, noun)| format!("{count} {noun}")),
            );
        }
    }

    format!(
        "rr {} — {}{fallback}, {} ({} ms)",
        command.as_str(),
        report.outcome.as_str(),
        counters.join(", "),
        report.elapsed_ms
    )
}

/// Renders one refresh report as the compact JSON object.
///
/// # Errors
/// Returns a serialization error, which for this fixed shape means the
/// serializer itself failed rather than the data being unrepresentable.
pub fn render_refresh_json(
    report: &RefreshReport,
    command: RefreshCommand,
) -> Result<String, serde_json::Error> {
    // Serialized through one envelope so the schema version and command sit in
    // the same object as the counters without `RefreshReport` having to carry
    // CLI-shaped fields it would otherwise never use.
    #[derive(serde::Serialize)]
    struct Envelope<'report> {
        schema_version: u32,
        command: &'static str,
        #[serde(flatten)]
        report: &'report RefreshReport,
    }

    serde_json::to_string(&Envelope {
        schema_version: REPORT_SCHEMA_VERSION,
        command: command.as_str(),
        report,
    })
}

// --- status -----------------------------------------------------------------

/// How the working tree relates to the index and `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitLabel {
    /// Tracked, staged, and untracked state all agree with `HEAD`.
    Clean,
    /// Something differs.
    Dirty,
    /// At least one path has unmerged index stages.
    Conflicted,
    /// There is a repository, but its state could not be observed.
    Unavailable,
    /// The root is not inside a Git repository.
    NoGit,
}

impl GitLabel {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Conflicted => "conflicted",
            Self::Unavailable => "unavailable",
            Self::NoGit => "no-git",
        }
    }
}

/// How the published snapshot relates to the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotLabel {
    /// An incremental refresh would take the no-op path right now.
    Fresh,
    /// A refresh would rebuild and publish.
    Stale,
    /// Nothing has been published yet.
    Missing,
    /// A snapshot exists but does not survive strict validation.
    Corrupt,
    /// A snapshot exists but this binary cannot interpret it.
    Incompatible,
    /// A snapshot exists, and whether it is current cannot be determined.
    Unknown,
}

impl SnapshotLabel {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::Incompatible => "incompatible",
            Self::Unknown => "unknown",
        }
    }
}

/// A read-only view of repository and snapshot agreement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusReport {
    /// Working tree state.
    pub git: GitLabel,
    /// Current `HEAD`, when there is a born one.
    pub head: Option<Oid>,
    /// Snapshot state.
    pub snapshot: SnapshotLabel,
    /// How many paths a refresh would reconsider, or `None` when that cannot
    /// be known — which is not the same as zero.
    pub stale_paths: Option<u64>,
    /// References and imports the snapshot could not resolve.
    pub unresolved: u64,
}

/// Renders one status report as the single-line human summary.
#[must_use]
pub fn render_status_text(report: &StatusReport) -> String {
    let head = report
        .head
        .map_or_else(String::new, |head| format!(" @ {}", short_oid(head)));
    let git = format!("git: {}{head}", report.git.as_str());

    let snapshot = match (report.snapshot, report.stale_paths) {
        (SnapshotLabel::Stale, Some(paths)) => {
            format!(
                "snapshot: stale ({paths} {})",
                plural(paths, "path", "paths")
            )
        }
        (label, _) => format!("snapshot: {}", label.as_str()),
    };

    format!("{git} · {snapshot} · unresolved: {}", report.unresolved)
}

/// Renders one status report as the compact JSON object.
///
/// # Errors
/// Returns a serialization error from `serde_json`.
pub fn render_status_json(report: &StatusReport) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Envelope<'report> {
        schema_version: u32,
        command: &'static str,
        #[serde(flatten)]
        report: &'report StatusReport,
    }

    serde_json::to_string(&Envelope {
        schema_version: REPORT_SCHEMA_VERSION,
        command: "status",
        report,
    })
}

/// The first seven hex digits, the length Git itself abbreviates to by default.
fn short_oid(oid: Oid) -> String {
    let hex = oid.to_hex();
    hex.chars().take(7).collect()
}

const fn plural(count: u64, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

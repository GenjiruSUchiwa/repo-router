//! What a refresh or status run reports, and the two ways it is spelled.
//!
//! Text and JSON are two renderings of one value. Neither renderer recomputes a
//! counter or re-derives freshness: an agent parsing JSON and a human reading
//! the summary must never be able to disagree about what happened.

use crate::oid::Oid;
use crate::text::{Conflict, SymbolsState, TextReport};

use super::{FullReason, RefreshOutcome};

/// Version of the `rr refresh` / `rr map` JSON contract.
///
/// One number per command surface, and this one covers both because they are
/// one report. The rule for when it moves is [`crate::json_contract`]; the log
/// of what each value meant is there too, not here, so that #14's surfaces read
/// one page instead of three doc comments.
pub const REFRESH_SCHEMA_VERSION: u32 = 4;

/// Version of the `rr status` JSON contract.
///
/// Seeded at 3, not 1: 3 is what `rr status --json` published while it shared a
/// constant with `refresh`, and a version that went backwards would be a
/// stronger claim than any this split is making.
pub const STATUS_SCHEMA_VERSION: u32 = 3;

/// The one text spelling of the tags-tier counter.
///
/// Shared with every renderer so the human summary and the JSON field cannot
/// drift apart: the JSON key is `tags`, and so is this.
pub const TAGS_COUNTER_LABEL: &str = "tags";

/// The text spelling of the tags-with-parse-errors counter.
pub const TAGS_RECOVERED_COUNTER_LABEL: &str = "tags recovered";

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
    /// Files whose definitions came from a grammar's tags query over a clean
    /// parse tree.
    pub tags: u64,
    /// Files whose definitions came from a grammar's tags query over a tree
    /// with parse errors — the tags-tier analogue of a recovered parse.
    pub tags_recovered: u64,
    /// Whether the snapshot file was replaced.
    pub snapshot_updated: bool,
    /// Wall-clock duration of the run.
    pub elapsed_ms: u64,
}

/// One run of `rr refresh` or `rr map`, both halves of it.
///
/// The renderers take this and nothing else. A renderer that took the snapshot
/// half alone is how `--json` came to report `unchanged` while it rewrote every
/// committed map; there is now no value a renderer can be handed that omits the
/// other half.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunReport {
    /// What the snapshot pass did.
    pub refresh: RefreshReport,
    /// What the text-artifact pass did.
    pub text: TextReport,
}

impl RunReport {
    /// What the run did, taken as a whole.
    ///
    /// Refusal first: a run that refused wrote nothing, so no counter below it
    /// can be non-zero, and saying `updated` about a repository that is exactly
    /// as it was found would be the same failure in a new place.
    #[must_use]
    pub fn outcome(&self) -> RefreshOutcome {
        if !self.text.conflicts.is_empty() {
            return RefreshOutcome::Refused;
        }
        if self.refresh.snapshot_updated || self.text.changed() {
            return RefreshOutcome::Updated;
        }
        RefreshOutcome::Unchanged
    }
}

/// How much of a report the JSON object carries.
///
/// An enum rather than a `bool` because `render_refresh_json(&run, command,
/// true)` says nothing at the call site, and because the set is closed: there
/// is no third answer that is not a new surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportDetail {
    /// Counters and conflicts.
    Summary,
    /// Those, and the per-path breakdown behind them.
    Verbose,
}

/// Renders one run as the single-line human summary.
///
/// Counters that are zero and had nothing to say are omitted, so the line stays
/// about what happened rather than about what did not. The text clause is
/// appended here and not by the caller: a caller that could forget it is how
/// `--json` came to omit it entirely.
#[must_use]
pub fn render_refresh_text(report: &RunReport, command: RefreshCommand) -> String {
    let snapshot = &report.refresh;
    let outcome = report.outcome();
    let fallback = snapshot.fallback_reason.map_or_else(String::new, |reason| {
        format!(" (full fallback: {})", reason.as_text())
    });

    let mut counters = vec![format!("{} reparsed", snapshot.reparsed)];
    match outcome {

        RefreshOutcome::Unchanged => {
            counters.push(format!("{} content reads", snapshot.content_reads));
        }
        RefreshOutcome::Updated => {
            counters.push(format!("{} cached", snapshot.cached));
            counters.extend(
                [
                    (snapshot.removed, "removed"),
                    (snapshot.renamed, "renamed"),
                    (snapshot.degraded, "degraded"),
                    (snapshot.tags, TAGS_COUNTER_LABEL),
                    (snapshot.tags_recovered, TAGS_RECOVERED_COUNTER_LABEL),
                    (snapshot.conflicted, "conflicted"),
                    (snapshot.cache_corrupt, "cache corrupt"),
                ]
                .into_iter()
                .filter(|&(count, _)| count > 0)
                .map(|(count, noun)| format!("{count} {noun}")),
            );
        }
        RefreshOutcome::Refused => {
            counters.push(format!("{} conflicting", report.text.conflicts.len()));
        }
    }

    format!(
        "rr {} — {}{fallback}, {} ({} ms){}",
        command.as_str(),
        outcome.as_str(),
        counters.join(", "),
        snapshot.elapsed_ms,
        report.text.clause()
    )
}

/// The counter breakdown behind the summary, and the paths behind the clause.
///
/// The third renderer of the same value, and it lives here for the reason the
/// other two do: a per-path block assembled at the call site is a block the
/// `--json` surface can be shipped without.
#[must_use]
pub fn render_refresh_verbose(report: &RunReport) -> String {
    let snapshot = &report.refresh;
    let mut out = format!(
        "  plan: {} mode, {} changed, {} removed, {} renamed, {} conflicted\n  \
         work: {} reparsed, {} cached, {} content reads, {} degraded, {} {}, {} {}\n  \
         cache: {} corrupt, {} routes retired\n  snapshot: {}",
        snapshot.mode.as_str(),
        snapshot.changed,
        snapshot.removed,
        snapshot.renamed,
        snapshot.conflicted,
        snapshot.reparsed,
        snapshot.cached,
        snapshot.content_reads,
        snapshot.degraded,
        snapshot.tags,
        TAGS_COUNTER_LABEL,
        snapshot.tags_recovered,
        TAGS_RECOVERED_COUNTER_LABEL,
        snapshot.cache_corrupt,
        report.text.routes_retired,
        if snapshot.snapshot_updated {
            "republished"
        } else {
            "unchanged on disk"
        }
    );
    let paths = report.text.verbose_lines();
    if !paths.is_empty() {
        out.push('\n');
        out.push_str(&paths);
    }
    out
}

/// Renders one run as the compact JSON object.
///
/// Every key is named here rather than inherited through `#[serde(flatten)]`,
/// for a reason that is not style: a flattened struct makes the contract's key
/// set whatever the struct's fields happen to be, so an unrelated edit to
/// `RefreshReport` publishes a key. [`crate::json_contract`] is the rule this
/// object follows.
///
/// # Errors
/// Returns a serialization error, which for this fixed shape means the
/// serializer itself failed rather than the data being unrepresentable.
pub fn render_refresh_json(
    report: &RunReport,
    command: RefreshCommand,
    detail: ReportDetail,
) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Envelope<'report> {
        schema_version: u32,
        command: &'static str,
        outcome: RefreshOutcome,
        mode: ReportedMode,
        fallback_reason: Option<FullReason>,
        changed: u64,
        reparsed: u64,
        cached: u64,
        cache_corrupt: u64,
        content_reads: u64,
        removed: u64,
        renamed: u64,
        degraded: u64,
        tags: u64,
        tags_recovered: u64,
        conflicted: u64,
        snapshot_updated: bool,
        text: Text<'report>,
        elapsed_ms: u64,
    }

    /// The text half. `written_paths` and `removed_paths` are absent rather
    /// than empty under `ReportDetail::Summary`: rr's own repository generates
    /// hundreds of committed maps, and a report that carried every path on
    /// every run would be more verbose by default than the human line is.
    #[derive(serde::Serialize)]
    struct Text<'report> {
        written: u64,
        unchanged: u64,
        removed: u64,
        symbols: SymbolsState,
        pending_purposes: u32,
        over_budget: &'report [String],
        conflicts: &'report [Conflict],
        #[serde(skip_serializing_if = "Option::is_none")]
        written_paths: Option<&'report [String]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        removed_paths: Option<&'report [String]>,
    }

    let verbose = detail == ReportDetail::Verbose;
    let snapshot = &report.refresh;
    serde_json::to_string(&Envelope {
        schema_version: REFRESH_SCHEMA_VERSION,
        command: command.as_str(),
        outcome: report.outcome(),
        mode: snapshot.mode,
        fallback_reason: snapshot.fallback_reason,
        changed: snapshot.changed,
        reparsed: snapshot.reparsed,
        cached: snapshot.cached,
        cache_corrupt: snapshot.cache_corrupt,
        content_reads: snapshot.content_reads,
        removed: snapshot.removed,
        renamed: snapshot.renamed,
        degraded: snapshot.degraded,
        tags: snapshot.tags,
        tags_recovered: snapshot.tags_recovered,
        conflicted: snapshot.conflicted,
        snapshot_updated: snapshot.snapshot_updated,
        text: Text {
            written: report.text.written,
            unchanged: report.text.unchanged,
            removed: report.text.removed,
            symbols: report.text.symbols,
            pending_purposes: report.text.pending_purposes,
            over_budget: &report.text.over_budget,
            conflicts: &report.text.conflicts,
            written_paths: verbose.then_some(report.text.written_paths.as_slice()),
            removed_paths: verbose.then_some(report.text.removed_paths.as_slice()),
        },
        elapsed_ms: snapshot.elapsed_ms,
    })
}

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
        schema_version: STATUS_SCHEMA_VERSION,
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

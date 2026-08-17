//! Every question `rr check` asks, and nothing it does about the answers.
//!
//! One pass over a repository's artifacts, producing a list of numbered
//! diagnostics and an exit code. **It is read-only**: see [`check`].
//!
//! # Every rule delegates
//!
//! No rule in this module reads an artifact for itself. Each one calls the
//! module that owns the artifact and reports what that module concluded — the
//! snapshot envelope through [`SnapshotStore`], the committed text through
//! [`validate_text_artifacts`], the learned routes through [`load_routes`] and
//! [`resolve_route_anchor`], the fact cache through [`FactCache`], the corpus
//! evidence through [`crate::quality`]. A second parser here would be a second
//! opinion, and the whole value of a checker is that it agrees with the code it
//! is checking: a rule that disagreed with its owner would report a repository
//! as broken that every other command handles, or pass one that `rr map`
//! refuses.
//!
//! That is also why `class_of` has no wildcard arm. `ConflictReason` is a
//! closed set precisely so that adding a variant is a compile error here, and
//! a `_` would trade that for a new kind of conflict silently reported under
//! whichever rule happened to come last.
//!
//! # Rule numbering
//!
//! The number is the contract; the suffix is prose for a human reading a log.
//! Families: `RR00xx` the snapshot, `RR01xx` committed maps, `RR02xx` the local
//! symbol index, `RR03xx` the learned route cache, `RR04xx` rebuildable local
//! caches, `RR05xx` the corpus evidence ([`crate::quality`]), `RR06xx` the agent
//! contract and the managed ignore block. Ids are never renumbered and never
//! reused: a CI pipeline greps for them.
//!
//! Some ids are declared and deliberately emit nothing — see
//! [`RESERVED_RULES`]. They are declared anyway so a later release answers them
//! without moving the ids around a pipeline already matches on.
//!
//! # What is deliberately not a rule
//!
//! There is no rule keyed on [`crate::index::Snapshot::unresolved_count`], and
//! there must never be one. That number sums the references and imports the
//! index could not resolve, and for whole languages the honest answer is "all of
//! them": a Python or TypeScript repository resolves no imports by path at all.
//! A threshold on it would fail those repositories on a green day, for a
//! population that is unresolvable by design rather than by neglect. It is a
//! reported figure and never a criterion.

use std::path::Path;

use crate::cache::{CacheKey, CacheOutcome, FactCache};
use crate::cancel::CancelToken;
use crate::facts::Facts;
use crate::quality::{self, QualitySummary};
use crate::query::resolve_route_anchor;
use crate::refresh::SnapshotLabel;
use crate::snapshot::{LoadOutcome, RebuildReason, SnapshotStore};
use crate::text::{
    api_identity, load_routes, validate_text_artifacts, ConflictReason, RouteFault, RouteRecord,
    DEFAULT_MAP_BUDGET, IGNORE_PATH, ROUTES_PATH, SYMBOLS_PATH,
};

/// Version of the `check` JSON contract.
///
/// Its own number rather than a shared one: the rule is
/// [`crate::json_contract`], and a counter added to `rr refresh` must not make
/// a `check` consumer re-validate an object that did not change.
pub const CHECK_SCHEMA_VERSION: u32 = 1;

/// The command name this surface publishes.
pub const CHECK_COMMAND: &str = "check";

/// Nothing is published yet, so there is nothing to check against.
pub const RR0001_SNAPSHOT_MISSING: &str = "RR0001_SNAPSHOT_MISSING";
/// The snapshot exists and its bytes do not survive strict validation.
pub const RR0002_SNAPSHOT_CORRUPT: &str = "RR0002_SNAPSHOT_CORRUPT";
/// The snapshot exists and this binary cannot interpret it.
pub const RR0003_SNAPSHOT_INCOMPATIBLE: &str = "RR0003_SNAPSHOT_INCOMPATIBLE";
/// The working tree has moved on from what the snapshot describes.
pub const RR0004_SNAPSHOT_STALE: &str = "RR0004_SNAPSHOT_STALE";

/// A committed `MAP.md` the generation would write is not on disk.
pub const RR0101_MAP_MISSING: &str = "RR0101_MAP_MISSING";
/// rr owns a committed map and its contents no longer parse as what rr wrote.
pub const RR0102_MAP_INVALID: &str = "RR0102_MAP_INVALID";
/// A committed map is rr's, intact, and describes an older projection.
pub const RR0103_MAP_STALE: &str = "RR0103_MAP_STALE";
/// A scope has no page that fits the map budget.
pub const RR0104_MAP_OVER_BUDGET: &str = "RR0104_MAP_OVER_BUDGET";
/// Reserved. See [`RESERVED_RULES`].
pub const RR0105_MAP_RECORD_INDIVISIBLE: &str = "RR0105_MAP_RECORD_INDIVISIBLE";
/// rr must not write a committed map's path at all.
pub const RR0106_MAP_NOT_WRITABLE: &str = "RR0106_MAP_NOT_WRITABLE";

/// `.rr/SYMBOLS.md` is absent.
pub const RR0201_SYMBOLS_MISSING: &str = "RR0201_SYMBOLS_MISSING";
/// `.rr/SYMBOLS.md` is rr's and no longer parses as what rr wrote.
pub const RR0202_SYMBOLS_INVALID: &str = "RR0202_SYMBOLS_INVALID";
/// `.rr/SYMBOLS.md` is rr's, intact, and describes an older projection.
pub const RR0203_SYMBOLS_STALE: &str = "RR0203_SYMBOLS_STALE";
/// rr must not write `.rr/SYMBOLS.md` at all.
pub const RR0206_SYMBOLS_NOT_WRITABLE: &str = "RR0206_SYMBOLS_NOT_WRITABLE";

/// `.rr/ROUTES.md` could not be read and has been discarded.
pub const RR0301_ROUTES_INVALID: &str = "RR0301_ROUTES_INVALID";
/// A learned route names an anchor the snapshot no longer holds.
pub const RR0302_ROUTE_ANCHOR_MISSING: &str = "RR0302_ROUTE_ANCHOR_MISSING";
/// The route cache was learned against another corpus API identity.
pub const RR0303_ROUTE_API_STALE: &str = "RR0303_ROUTE_API_STALE";

/// A rebuildable local cache entry does not decode.
pub const RR0401_CACHE_CORRUPT: &str = "RR0401_CACHE_CORRUPT";

/// Reserved. See [`RESERVED_RULES`].
pub const RR0601_CONTRACT_MISSING: &str = "RR0601_CONTRACT_MISSING";
/// Reserved. See [`RESERVED_RULES`].
pub const RR0602_CONTRACT_NOT_OWNED: &str = "RR0602_CONTRACT_NOT_OWNED";
/// Reserved. See [`RESERVED_RULES`].
pub const RR0603_CONTRACT_STALE: &str = "RR0603_CONTRACT_STALE";
/// The managed block in `.rr/.gitignore` is duplicated or malformed.
pub const RR0604_LOCAL_IGNORE_INVALID: &str = "RR0604_LOCAL_IGNORE_INVALID";

/// Every rule id this module can emit, in numeric order.
///
/// Published so a pipeline can enumerate the contract instead of discovering it
/// from failures, and so that [`RESERVED_RULES`] being disjoint from it is a
/// checkable claim rather than a comment. The `RR05xx` ids belong to
/// [`crate::quality`] and are listed by [`crate::quality::ADJUDICABLE_RULES`]
/// there.
pub const EMITTED_RULES: [&str; 18] = [
    RR0001_SNAPSHOT_MISSING,
    RR0002_SNAPSHOT_CORRUPT,
    RR0003_SNAPSHOT_INCOMPATIBLE,
    RR0004_SNAPSHOT_STALE,
    RR0101_MAP_MISSING,
    RR0102_MAP_INVALID,
    RR0103_MAP_STALE,
    RR0104_MAP_OVER_BUDGET,
    RR0106_MAP_NOT_WRITABLE,
    RR0201_SYMBOLS_MISSING,
    RR0202_SYMBOLS_INVALID,
    RR0203_SYMBOLS_STALE,
    RR0206_SYMBOLS_NOT_WRITABLE,
    RR0301_ROUTES_INVALID,
    RR0302_ROUTE_ANCHOR_MISSING,
    RR0303_ROUTE_API_STALE,
    RR0401_CACHE_CORRUPT,
    RR0604_LOCAL_IGNORE_INVALID,
];

/// Ids that are declared, numbered, and emit nothing in this release.
///
/// Each is here for a reason about *ownership*, not about effort, and neither
/// reason is fixed by writing more code in this file:
///
/// - [`RR0105_MAP_RECORD_INDIVISIBLE`] would separate "one record is larger
///   than a whole page" from "the budget cannot even hold the lines saying what
///   was dropped". The owning projection collapses both into one boolean —
///   `ScopePlan::is_over_budget`, whose parts are crate-private — and it
///   publishes only the scope list [`RR0104_MAP_OVER_BUDGET`] already reports.
///   Splitting the two here would mean re-deriving the page arithmetic beside
///   its owner, which is the one thing this module refuses to do. Both causes
///   therefore surface as `RR0104`, a warning, and this id waits for the
///   projection to publish the distinction.
/// - [`RR0601_CONTRACT_MISSING`], [`RR0602_CONTRACT_NOT_OWNED`] and
///   [`RR0603_CONTRACT_STALE`] would ask whether `rr init`'s agent contract is
///   installed, still rr's, and current. Every answer lives in
///   `crates/rr-cli/src/init.rs`, private, and on a *write* path:
///   `crate::text::apply_block` computes the new bytes and hands them straight
///   to the writer. `check` lives in this crate, which `rr-cli` depends on, so
///   the call cannot go that way — the only honest fix is to extract a
///   read-only `init` planner into `rr-core`, which changes a shipped command
///   and is its own piece of work. The alternative is a second parser for the
///   contract block, which is exactly what this module exists without.
pub const RESERVED_RULES: [&str; 4] = [
    RR0105_MAP_RECORD_INDIVISIBLE,
    RR0601_CONTRACT_MISSING,
    RR0602_CONTRACT_NOT_OWNED,
    RR0603_CONTRACT_STALE,
];

/// How much a diagnostic costs its repository.
///
/// Variant order is the severity order, so sorting a diagnostic list ascending
/// puts the fatal one first. Three levels and no `Info`: a checker that had a
/// level nothing acts on would grow one, and then the exit code would stop
/// meaning anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Nothing further can be checked.
    Fatal,
    /// The repository is wrong in a way a human or `rr map` must fix.
    Error,
    /// The repository works and something is out of step.
    Warning,
}

impl Severity {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One numbered finding about one repository.
///
/// Every `Option` is `skip_serializing_if`, so a consumer reads a key's absence
/// as "not applicable" rather than as `null`.
///
/// `path` is repository-relative. An absolute one would name the machine that
/// ran the check — a home directory, a runner's workspace id — inside an
/// artifact a CI job publishes, which is a leak nobody chose and nobody
/// notices.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DiagnosticV1 {
    /// The rule that fired, from [`EMITTED_RULES`] or
    /// [`crate::quality::ADJUDICABLE_RULES`].
    pub rule_id: &'static str,
    /// How much it costs.
    pub severity: Severity,
    /// The one-line explanation, in the owning module's own spelling.
    pub message: String,
    /// The repository-relative path the finding is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The `rr query` anchor the finding is about, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// What the checker required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// What it found, in the spelling the owning enum publishes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    /// The command or edit that resolves it.
    pub remediation: &'static str,
}

impl DiagnosticV1 {
    /// The total order both renderers see.
    ///
    /// Total over every field a reader can see, so two diagnostics can never
    /// compare equal while printing differently — which is what would make the
    /// sort's result depend on the order the rules happened to run in.
    fn sort_key(&self) -> impl Ord + '_ {
        (
            self.severity,
            self.rule_id,
            self.path.as_deref(),
            self.anchor.as_deref(),
            self.message.as_str(),
            self.expected.as_deref(),
            self.actual.as_deref(),
        )
    }
}

/// The verdict on one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    /// Nothing to report.
    Ok,
    /// Warnings only.
    Warnings,
    /// At least one error.
    Errors,
    /// [`RR0001_SNAPSHOT_MISSING`]; nothing else could be asked.
    SnapshotMissing,
}

impl CheckStatus {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warnings => "warnings",
            Self::Errors => "errors",
            Self::SnapshotMissing => "snapshot-missing",
        }
    }

    /// The process exit code for this verdict.
    ///
    /// `0` nothing, `1` warnings, `3` at least one error, `4` no snapshot at
    /// all, and precedence runs `4 > 3 > 1 > 0`.
    ///
    /// **`2` is `clap`'s and is never returned here.** The gap is the whole
    /// point: `2` already means "you mistyped the invocation", so spending it
    /// on an error-severity finding would leave a CI script unable to tell a
    /// misspelled `--quality-report` from a broken repository — and only one of
    /// those pages a human. A code is shared only when the two meanings
    /// coincide, which is why the failure path of `rr check` does return `2`:
    /// a crash and a mistyped flag both mean the invocation could not be
    /// evaluated.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Warnings => 1,
            Self::Errors => 3,
            Self::SnapshotMissing => 4,
        }
    }
}

/// How many diagnostics of each severity one check produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct CheckCounts {
    /// [`Severity::Fatal`] diagnostics.
    pub fatal: u32,
    /// [`Severity::Error`] diagnostics.
    pub errors: u32,
    /// [`Severity::Warning`] diagnostics.
    pub warnings: u32,
}

impl CheckCounts {
    /// The verdict these counts imply, at the locked precedence.
    #[must_use]
    pub const fn status(self) -> CheckStatus {
        if self.fatal > 0 {
            CheckStatus::SnapshotMissing
        } else if self.errors > 0 {
            CheckStatus::Errors
        } else if self.warnings > 0 {
            CheckStatus::Warnings
        } else {
            CheckStatus::Ok
        }
    }
}

/// One `rr check` run, as both renderers see it.
///
/// Declaration order is JSON field order, and `schema_version` is first, per
/// [`crate::json_contract`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckResultV1 {
    /// [`CHECK_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The verdict.
    pub status: CheckStatus,
    /// The per-severity totals.
    pub counts: CheckCounts,
    /// Every finding, in one total order over every field a reader can see.
    pub diagnostics: Vec<DiagnosticV1>,
    /// What a `--quality-report` contributed, when one was given and admitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<QualitySummary>,
}

impl CheckResultV1 {
    /// The process exit code for this run.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.status.exit_code()
    }
}

/// Validates the repository at `root`, changing nothing about it.
///
/// **Read-only, and that is a contract rather than an implementation detail:
/// this never repairs, refreshes, quarantines, rewrites or deletes anything.**
/// It publishes no snapshot, writes no map, files no memo, resets no cache and
/// creates no directory. A checker that fixed what it found would make its own
/// second run report a clean repository, so a CI job could never distinguish "it
/// was fine" from "it was broken and something silently changed the tree the
/// build is about to ship".
///
/// `label` is the working tree's verdict on the snapshot, from `rr_git::status`.
/// It is a parameter and not a call because the module that answers it depends
/// on this crate rather than the other way round; `None` means the question
/// could not be put — the repository is not a Git repository, or its tree could
/// not be compared — and then [`RR0004_SNAPSHOT_STALE`] is simply not evaluated,
/// which is honest where claiming freshness would not be. Only the label is
/// taken, never the whole `StatusReport`: that report also carries
/// `unresolved`, and a rule keyed on it is the mistake this module's header
/// forbids.
///
/// `quality` is opt-in. It *adds* the corpus verdicts to the repository's own
/// diagnostics and suppresses none of them: ordinary repositories have no
/// corpus, and a repository that has one is still a repository.
///
/// # Errors
/// Returns an error only when the check itself could not be carried out — the
/// snapshot file could not be read for a reason other than absence, or the
/// snapshot could not be projected. A broken artifact is a diagnostic, which is
/// the point of the command; and cancellation is an
/// [`std::io::ErrorKind::Interrupted`], never a clean report about work that was
/// never done.
pub fn check(
    root: &Path,
    label: Option<SnapshotLabel>,
    quality: Option<&Path>,
    cancel: &CancelToken,
) -> crate::Result<CheckResultV1> {
    let mut diagnostics = Vec::new();

    let outcome = SnapshotStore::new(root)
        .load()
        .map_err(|error| crate::Error::Io(std::io::Error::other(error)))?;

    match outcome {
        LoadOutcome::Missing => diagnostics.push(snapshot_missing()),
        LoadOutcome::NeedsRebuild(reason) => diagnostics.push(unusable_snapshot(&reason)),
        LoadOutcome::Ready(snapshot) => {
            if label == Some(SnapshotLabel::Stale) {
                diagnostics.push(stale_snapshot());
            }
            interrupted(cancel)?;
            text_diagnostics(root, &snapshot, &mut diagnostics)?;
            interrupted(cancel)?;
            route_diagnostics(root, &snapshot, &mut diagnostics)?;
            interrupted(cancel)?;
            cache_diagnostics(root, &snapshot, &mut diagnostics);
        }
    }

    interrupted(cancel)?;
    let summary = quality.map(quality::adjudicate).map(|adjudication| {
        diagnostics.extend(adjudication.diagnostics);
        adjudication.summary
    });

    diagnostics.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    let counts = counts_of(&diagnostics);
    Ok(CheckResultV1 {
        schema_version: CHECK_SCHEMA_VERSION,
        status: counts.status(),
        counts,
        diagnostics,
        quality: summary.flatten(),
    })
}

/// Stops the run when the caller asked it to.
///
/// An error rather than a short report: a partial answer that still said `ok`
/// would be the one output of this command nobody could act on.
fn interrupted(cancel: &CancelToken) -> crate::Result<()> {
    if cancel.is_cancelled() {
        return Err(crate::Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "check was cancelled",
        )));
    }
    Ok(())
}

/// The per-severity totals of a finished diagnostic list.
fn counts_of(diagnostics: &[DiagnosticV1]) -> CheckCounts {
    let mut counts = CheckCounts::default();
    for diagnostic in diagnostics {
        let slot = match diagnostic.severity {
            Severity::Fatal => &mut counts.fatal,
            Severity::Error => &mut counts.errors,
            Severity::Warning => &mut counts.warnings,
        };
        *slot = slot.saturating_add(1);
    }
    counts
}

/// [`RR0001_SNAPSHOT_MISSING`]: fatal, because every other rule needs it.
fn snapshot_missing() -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0001_SNAPSHOT_MISSING,
        severity: Severity::Fatal,
        message: String::from("no snapshot has been published for this repository"),
        path: None,
        anchor: None,
        expected: None,
        actual: Some(String::from(SnapshotLabel::Missing.as_str())),
        remediation: "run `rr map`",
    }
}

/// [`RR0002_SNAPSHOT_CORRUPT`] or [`RR0003_SNAPSHOT_INCOMPATIBLE`].
///
/// The split is *damage* against *vintage*, and it is worth two rule ids
/// because the actions differ in kind: a corrupt snapshot may mean a truncated
/// write or a bad disk and is worth looking into, while an incompatible one is
/// the ordinary consequence of upgrading `rr` and is fixed by rebuilding.
/// Exhaustive over [`RebuildReason`] with no wildcard, for the reason the module
/// header gives.
fn unusable_snapshot(reason: &RebuildReason) -> DiagnosticV1 {
    let (rule_id, message, actual) = match reason {
        RebuildReason::BadMagic => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot does not begin with the snapshot magic",
            String::from("bad-magic"),
        ),
        RebuildReason::LengthMismatch => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot is shorter than its own header declares",
            String::from("length-mismatch"),
        ),
        RebuildReason::ChecksumMismatch => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot payload does not match its checksum",
            String::from("checksum-mismatch"),
        ),
        RebuildReason::InvalidPayload => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot payload does not decode",
            String::from("invalid-payload"),
        ),
        RebuildReason::TrailingBytes => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot has bytes after its payload",
            String::from("trailing-bytes"),
        ),
        RebuildReason::InvalidInvariant => (
            RR0002_SNAPSHOT_CORRUPT,
            "the snapshot decodes and violates its own invariants",
            String::from("invalid-invariant"),
        ),
        RebuildReason::UnsupportedVersion { found } => (
            RR0003_SNAPSHOT_INCOMPATIBLE,
            "the snapshot was written under another envelope version",
            format!("unsupported-version {found}"),
        ),
        RebuildReason::BuildVersionMismatch { found } => (
            RR0003_SNAPSHOT_INCOMPATIBLE,
            "the snapshot was built by another index build",
            format!("build-version-mismatch {found}"),
        ),
        RebuildReason::RankingProfileMismatch { found } => (
            RR0003_SNAPSHOT_INCOMPATIBLE,
            "the snapshot was ranked under another ranking profile",
            format!("ranking-profile-mismatch {found}"),
        ),
        RebuildReason::LexicalMismatch => (
            RR0003_SNAPSHOT_INCOMPATIBLE,
            "the snapshot was lexed under another lexical profile",
            String::from("lexical-mismatch"),
        ),
    };
    DiagnosticV1 {
        rule_id,
        severity: Severity::Error,
        message: String::from(message),
        path: None,
        anchor: None,
        expected: None,
        actual: Some(actual),
        remediation: "run `rr map` to rebuild the snapshot",
    }
}

/// [`RR0004_SNAPSHOT_STALE`]: a warning, because the snapshot still answers.
fn stale_snapshot() -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0004_SNAPSHOT_STALE,
        severity: Severity::Warning,
        message: String::from("the working tree has moved on from the snapshot"),
        path: None,
        anchor: None,
        expected: Some(String::from(SnapshotLabel::Fresh.as_str())),
        actual: Some(String::from(SnapshotLabel::Stale.as_str())),
        remediation: "run `rr refresh`",
    }
}

/// Which artifact one reported path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// A committed `MAP.md` or overflow page.
    Map,
    /// `.rr/SYMBOLS.md`.
    Symbols,
    /// `.rr/.gitignore`'s managed block.
    Ignore,
}

/// What kind of failure one conflict is, apart from which artifact it hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// rr must not write this path at all.
    NotWritable,
    /// rr owns the path and its contents no longer parse as what rr wrote.
    Malformed,
    /// Not an artifact: the managed block in `.rr/.gitignore`.
    ManagedIgnore,
}

/// Classify one conflict reason.
///
/// Deliberately wildcard-free: the closed set exists so this match is what the
/// compiler rechecks when a variant is added. A `_` arm hands back exactly the
/// failure the closed set was built to prevent — a new kind of conflict reported
/// under whichever rule happened to come last.
const fn class_of(reason: ConflictReason) -> Class {
    match reason {
        ConflictReason::NotOwned
        | ConflictReason::Symlink
        | ConflictReason::MergeConflict
        | ConflictReason::CaseCollision
        | ConflictReason::Unreadable => Class::NotWritable,
        ConflictReason::Frontmatter
        | ConflictReason::UnsupportedFormat
        | ConflictReason::Marker
        | ConflictReason::Purpose
        | ConflictReason::GeneratedEdited
        | ConflictReason::Anchor => Class::Malformed,
        ConflictReason::ManagedIgnore => Class::ManagedIgnore,
    }
}

/// Which family a reported path belongs to.
///
/// Two named constants and a default, because that is exactly what
/// `validate_text_artifacts` reports: `.rr/SYMBOLS.md`, `.rr/.gitignore`, and
/// committed map pages under whichever directories the projection covers. The
/// map arm is the default rather than a third test against
/// `is_reserved_artifact_name`, so a path the projection starts reporting is
/// filed under a rule that exists instead of falling into an "unknown" family
/// whose rule id nothing documents and no pipeline greps for.
fn family_of(path: &str) -> Family {
    match path {
        SYMBOLS_PATH => Family::Symbols,
        IGNORE_PATH => Family::Ignore,
        _ => Family::Map,
    }
}

/// The rule id one conflict is reported under.
///
/// Family from the path, class from the reason, and the pair names the rule.
/// The ignore block is decided by its path alone, whatever the class: it is not
/// an artifact of the text generation at all, and a reason raised against it is
/// always about its managed markers.
const fn rule_for(family: Family, class: Class) -> &'static str {
    match family {
        Family::Ignore => RR0604_LOCAL_IGNORE_INVALID,
        Family::Map => match class {
            Class::NotWritable => RR0106_MAP_NOT_WRITABLE,
            Class::Malformed | Class::ManagedIgnore => RR0102_MAP_INVALID,
        },
        Family::Symbols => match class {
            Class::NotWritable => RR0206_SYMBOLS_NOT_WRITABLE,
            Class::Malformed | Class::ManagedIgnore => RR0202_SYMBOLS_INVALID,
        },
    }
}

/// The edit that clears one conflict, per reason and not per class.
///
/// This is what the closed set buys that a message string could not: `NotOwned`
/// is answered by moving a file, `GeneratedEdited` by regenerating one, and no
/// amount of parsing a sentence would have told the two apart. Wildcard-free
/// for the same reason as [`class_of`].
const fn remediation_for(reason: ConflictReason) -> &'static str {
    match reason {
        ConflictReason::NotOwned => "move or delete the file, then run `rr map`",
        ConflictReason::Symlink => "replace the symbolic link with nothing, then run `rr map`",
        ConflictReason::MergeConflict => "finish the merge, then run `rr map`",
        ConflictReason::CaseCollision => "rename the differently-spelled file, then run `rr map`",
        ConflictReason::Unreadable => "restore read permission on the file, then run `rr map`",
        ConflictReason::Frontmatter | ConflictReason::UnsupportedFormat => {
            "delete the file and run `rr map` to write it under this format"
        }
        ConflictReason::Marker | ConflictReason::Anchor => {
            "restore the generated region, or delete the file and run `rr map`"
        }
        ConflictReason::Purpose => "shorten the purpose line, then run `rr map`",
        ConflictReason::GeneratedEdited => "run `rr map` to regenerate the file",
        ConflictReason::ManagedIgnore => {
            "resolve the duplicated or malformed rr markers in .rr/.gitignore"
        }
    }
}

/// The diagnostic one conflict is reported as.
///
/// Public because `rr refresh --json` publishes the same conflicts, and a
/// consumer that wanted to name them by rule id would otherwise rebuild this
/// table — the second opinion the module header rules out.
///
/// `message` and `actual` carry the same bytes on purpose.
/// [`ConflictReason::as_str`] *is* the published serde spelling, so a program
/// branching on `actual` and a human reading `message` are looking at one
/// string; giving the human a second rendering is precisely how the two start
/// to drift. The sentence a human wants is in `remediation`, which is per
/// reason.
#[must_use]
pub fn conflict_diagnostic(path: &str, reason: ConflictReason) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: rule_for(family_of(path), class_of(reason)),
        severity: Severity::Error,
        message: String::from(reason.as_str()),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: Some(String::from(reason.as_str())),
        remediation: remediation_for(reason),
    }
}

/// `RR01xx`, `RR02xx` and [`RR0604_LOCAL_IGNORE_INVALID`], from one
/// [`validate_text_artifacts`] call.
///
/// One call and not four, so every list describes the same generation: two calls
/// could see two projections and report a file as both stale and fresh.
///
/// # Errors
/// Returns what [`validate_text_artifacts`] returns — a snapshot that cannot be
/// projected at all, which is a failure of the check rather than a finding.
fn text_diagnostics(
    root: &Path,
    snapshot: &crate::index::Snapshot,
    diagnostics: &mut Vec<DiagnosticV1>,
) -> crate::Result<()> {
    let validation = validate_text_artifacts(snapshot, root, DEFAULT_MAP_BUDGET)?;

    for conflict in validation.conflicts() {
        diagnostics.push(conflict_diagnostic(conflict.path(), conflict.reason()));
    }
    for path in validation.missing() {
        diagnostics.push(absent_artifact(path));
    }
    for path in validation.stale() {
        diagnostics.push(stale_artifact(path));
    }
    for scope in validation.over_budget() {
        diagnostics.push(over_budget(scope));
    }
    Ok(())
}

/// [`RR0101_MAP_MISSING`] or [`RR0201_SYMBOLS_MISSING`]: an error.
///
/// An error and not a warning, unlike its stale sibling: a stale map still
/// answers the question an agent came with, and an absent one answers nothing.
/// A repository whose committed navigation is simply not there is broken for
/// every reader, not merely behind.
fn absent_artifact(path: &str) -> DiagnosticV1 {
    let rule_id = match family_of(path) {
        Family::Symbols => RR0201_SYMBOLS_MISSING,
        Family::Map | Family::Ignore => RR0101_MAP_MISSING,
    };
    DiagnosticV1 {
        rule_id,
        severity: Severity::Error,
        message: String::from("the generation would write this artifact and it is not there"),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: None,
        remediation: "run `rr map`",
    }
}

/// [`RR0103_MAP_STALE`] or [`RR0203_SYMBOLS_STALE`]: a warning.
fn stale_artifact(path: &str) -> DiagnosticV1 {
    let rule_id = match family_of(path) {
        Family::Symbols => RR0203_SYMBOLS_STALE,
        Family::Map | Family::Ignore => RR0103_MAP_STALE,
    };
    DiagnosticV1 {
        rule_id,
        severity: Severity::Warning,
        message: String::from("this artifact describes an older projection"),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: None,
        remediation: "run `rr refresh`",
    }
}

/// [`RR0104_MAP_OVER_BUDGET`]: a warning, and both of its causes.
///
/// The message states both because the owning projection does not separate them;
/// see [`RESERVED_RULES`] on [`RR0105_MAP_RECORD_INDIVISIBLE`].
fn over_budget(scope: &str) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0104_MAP_OVER_BUDGET,
        severity: Severity::Warning,
        message: String::from(
            "no page of this scope fits the map budget: either a record is larger \
             than a whole page, or the budget cannot hold the lines stating what \
             was dropped",
        ),
        path: Some(scope.to_owned()),
        anchor: None,
        expected: None,
        actual: None,
        remediation: "raise the map budget, or split the oversize definition",
    }
}

/// `RR03xx`, the learned route cache.
///
/// # Errors
/// Returns what projecting the snapshot for its corpus API identity returns.
fn route_diagnostics(
    root: &Path,
    snapshot: &crate::index::Snapshot,
    diagnostics: &mut Vec<DiagnosticV1>,
) -> crate::Result<()> {
    let (table, fault) = load_routes(root);
    if let Some(fault) = fault {
        diagnostics.push(routes_invalid(fault));
    }
    if table.is_empty() {
        return Ok(());
    }

    for record in table.records() {
        if resolve_route_anchor(snapshot, &record.anchor).is_none() {
            diagnostics.push(route_anchor_missing(record));
        }
    }

    let corpus = api_identity(root, None, snapshot, DEFAULT_MAP_BUDGET)?;
    if let Some(stale) = table.records().find(|record| record.api_identity != corpus) {
        diagnostics.push(route_api_stale(&corpus.to_text(), stale));
    }
    Ok(())
}

/// [`RR0301_ROUTES_INVALID`]: a **warning**, not an error.
///
/// `RouteFault`'s own documentation settles the severity: every variant leads to
/// the same action — discard the file and start empty — and `load_routes` has
/// already done exactly that by the time this is written. A local cache that
/// rebuilds itself from the next query it answers is the class of
/// [`RR0401_CACHE_CORRUPT`], not the class of a committed artifact a human has
/// to repair. What the closed enum buys is preserved by putting the fault's own
/// spelling in `actual`, so a log says *which* reset happened.
fn routes_invalid(fault: RouteFault) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0301_ROUTES_INVALID,
        severity: Severity::Warning,
        message: String::from("the learned route cache could not be read and was discarded"),
        path: Some(String::from(ROUTES_PATH)),
        anchor: None,
        expected: None,
        actual: Some(String::from(fault.as_str())),
        remediation: "nothing: the cache refills itself as questions are asked again",
    }
}

/// [`RR0302_ROUTE_ANCHOR_MISSING`], once per record.
///
/// Per record and not per file, because each record is a separate dead answer a
/// separate question would receive. There is no status column to filter on:
/// a `RouteRecord` carries `key`, `anchor`, `map`, `api_identity` and
/// `confidence`, and nothing that says whether rr still believes it — which is
/// the question this rule exists to answer, by asking the resolver.
fn route_anchor_missing(record: &RouteRecord) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0302_ROUTE_ANCHOR_MISSING,
        severity: Severity::Warning,
        message: String::from("a learned route names an anchor the snapshot no longer holds"),
        path: Some(String::from(ROUTES_PATH)),
        anchor: Some(record.anchor.clone()),
        expected: Some(record.key.as_str().to_owned()),
        actual: None,
        remediation: "nothing: the route is ignored and relearned on the next ask",
    }
}

/// [`RR0303_ROUTE_API_STALE`], **once for the whole table**.
///
/// One diagnostic and not one per record, because `api_identity` is the identity
/// of the *corpus* rather than of a scope: every record rr wrote carries the
/// same value, so a cache of a thousand entries goes stale as one event. Emitting
/// a line each would make one rename look like a thousand defects and bury every
/// other finding in the report.
fn route_api_stale(corpus: &str, stale: &RouteRecord) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0303_ROUTE_API_STALE,
        severity: Severity::Warning,
        message: String::from("the route cache was learned against another corpus API identity"),
        path: Some(String::from(ROUTES_PATH)),
        anchor: None,
        expected: Some(corpus.to_owned()),
        actual: Some(stale.api_identity.to_text()),
        remediation: "nothing: every route is ignored and relearned on the next ask",
    }
}

/// [`RR0401_CACHE_CORRUPT`], the rebuildable fact cache.
///
/// Asked through [`FactCache::get`], which is the strict decoder that owns the
/// answer: a valid postcard prefix followed by anything is `Corrupt` there, and
/// re-deriving that here would be a second decoder that could forgive what the
/// real reader refuses.
///
/// The cache is opened only when it already exists, and that guard is the
/// read-only contract rather than an optimisation: [`FactCache::open`] creates
/// `.rr/local/facts`, restores the `.rr` ignore stamp if it is gone, and probes
/// for writability. A check must not bring a cache into being, nor put back a
/// file the user removed. When there is nothing to open, or the probe fails on a
/// read-only checkout, this rule reports nothing — silence about a rebuildable
/// cache costs one refresh, and a warning nobody can act on costs every run.
fn cache_diagnostics(
    root: &Path,
    snapshot: &crate::index::Snapshot,
    diagnostics: &mut Vec<DiagnosticV1>,
) {
    if !crate::workspace::facts_dir(root).is_dir() {
        return;
    }
    if !crate::workspace::state_dir(root)
        .join(".gitignore")
        .exists()
    {
        return;
    }
    let Ok(cache) = FactCache::open(root) else {
        return;
    };

    for file in &snapshot.files {
        let key = CacheKey::new(file.content_oid, file.language);
        if matches!(cache.get::<Facts>(&key), Ok(CacheOutcome::Corrupt)) {
            diagnostics.push(cache_corrupt(snapshot.string(file.path).unwrap_or("")));
        }
    }
}

/// [`RR0401_CACHE_CORRUPT`]: a warning, because the cache is rebuildable.
fn cache_corrupt(path: &str) -> DiagnosticV1 {
    DiagnosticV1 {
        rule_id: RR0401_CACHE_CORRUPT,
        severity: Severity::Warning,
        message: String::from("a fact cache entry does not decode and will be reparsed"),
        path: Some(path.to_owned()),
        anchor: None,
        expected: None,
        actual: Some(String::from("corrupt")),
        remediation: "nothing: the next refresh reparses the file and rewrites the entry",
    }
}

/// Renders one check as the human report.
///
/// One summary line, then one block per diagnostic in the same order the JSON
/// prints them, so a reader comparing the two never has to sort. No timestamp,
/// no elapsed time, no absolute path, no colour: two runs over one repository
/// are byte-identical, which is what makes the output diffable in a pull
/// request.
#[must_use]
pub fn render_check_text(result: &CheckResultV1) -> String {
    use std::fmt::Write as _;

    let mut out = format!(
        "check: {} · fatal: {} · errors: {} · warnings: {}\n",
        result.status.as_str(),
        result.counts.fatal,
        result.counts.errors,
        result.counts.warnings
    );
    if let Some(quality) = &result.quality {
        let _ = writeln!(
            out,
            "quality: schema {} · corpus {} · findings: {} · blocked: {}",
            quality.schema_version, quality.manifest_digest, quality.findings, quality.blocked
        );
    }
    for diagnostic in &result.diagnostics {
        match &diagnostic.path {
            Some(path) => {
                let _ = writeln!(
                    out,
                    "{} {} {}: {}",
                    diagnostic.severity.as_str(),
                    diagnostic.rule_id,
                    path,
                    diagnostic.message
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "{} {}: {}",
                    diagnostic.severity.as_str(),
                    diagnostic.rule_id,
                    diagnostic.message
                );
            }
        }
        for (label, value) in [
            ("anchor", diagnostic.anchor.as_deref()),
            ("expected", diagnostic.expected.as_deref()),
            ("actual", diagnostic.actual.as_deref()),
        ] {
            if let Some(value) = value {
                let _ = writeln!(out, "  {label}: {value}");
            }
        }
        let _ = writeln!(out, "  fix: {}", diagnostic.remediation);
    }
    out
}

/// Renders one check as the compact JSON object.
///
/// Field by field rather than by flattening the result, which is what keeps
/// `schema_version` first and `command` second — the shape every other report
/// surface publishes. `command` is the renderer's and not the value's: a
/// [`CheckResultV1`] does not know which invocation printed it.
///
/// # Errors
/// Returns a serialization error from `serde_json`.
pub fn render_check_json(result: &CheckResultV1) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Envelope<'result> {
        schema_version: u32,
        command: &'static str,
        status: CheckStatus,
        counts: CheckCounts,
        diagnostics: &'result [DiagnosticV1],
        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<&'result QualitySummary>,
    }

    serde_json::to_string(&Envelope {
        schema_version: result.schema_version,
        command: CHECK_COMMAND,
        status: result.status,
        counts: result.counts,
        diagnostics: &result.diagnostics,
        quality: result.quality.as_ref(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The twelve variants, listed by hand because the compiler has no opinion
    /// about a list. `class_of`'s missing wildcard is the real guard; this is
    /// what tells a maintainer *which* list to extend when that guard fires.
    const EVERY_REASON: [ConflictReason; 12] = [
        ConflictReason::NotOwned,
        ConflictReason::Symlink,
        ConflictReason::MergeConflict,
        ConflictReason::Frontmatter,
        ConflictReason::UnsupportedFormat,
        ConflictReason::Marker,
        ConflictReason::Purpose,
        ConflictReason::GeneratedEdited,
        ConflictReason::Anchor,
        ConflictReason::ManagedIgnore,
        ConflictReason::Unreadable,
        ConflictReason::CaseCollision,
    ];

    /// The list is not checked the way `test_support::assert_variant_count`
    /// checks the crate's other hand-written lists: that helper decodes a
    /// postcard variant index, and `ConflictReason` is `Serialize` only — it is
    /// published and never read back. What guards the list is [`class_of`]'s
    /// missing wildcard, at compile time; what this asserts is that the list
    /// itself does not name one variant twice and so pass while covering eleven.
    #[test]
    fn the_reason_list_names_each_variant_once() {
        let mut spellings: Vec<&str> = EVERY_REASON.iter().map(|r| r.as_str()).collect();
        spellings.sort_unstable();
        let listed = spellings.len();
        spellings.dedup();
        assert_eq!(
            spellings.len(),
            listed,
            "EVERY_REASON lists a variant twice"
        );
    }

    #[test]
    fn every_conflict_reason_maps_to_an_emitted_rule_and_is_an_error() {
        for reason in EVERY_REASON {
            for path in ["src/auth/MAP.md", SYMBOLS_PATH, IGNORE_PATH] {
                let diagnostic = conflict_diagnostic(path, reason);
                assert_eq!(
                    diagnostic.severity,
                    Severity::Error,
                    "{path} {reason:?} is not an error"
                );
                assert!(
                    EMITTED_RULES.contains(&diagnostic.rule_id),
                    "{path} {reason:?} produced the undeclared rule {}",
                    diagnostic.rule_id
                );
                assert!(
                    !diagnostic.remediation.is_empty(),
                    "{reason:?} has no remediation"
                );
            }
        }
    }

    /// Amendment G's split: an occupied reserved path and an edited generated
    /// one are different failures with different fixes, so they are different
    /// rules.
    #[test]
    fn an_occupied_path_and_an_edited_one_are_different_rules() {
        assert_eq!(
            conflict_diagnostic("src/auth/MAP.md", ConflictReason::NotOwned).rule_id,
            RR0106_MAP_NOT_WRITABLE
        );
        assert_eq!(
            conflict_diagnostic("src/auth/MAP.md", ConflictReason::GeneratedEdited).rule_id,
            RR0102_MAP_INVALID
        );
        assert_eq!(
            conflict_diagnostic(SYMBOLS_PATH, ConflictReason::Symlink).rule_id,
            RR0206_SYMBOLS_NOT_WRITABLE
        );
        assert_eq!(
            conflict_diagnostic(SYMBOLS_PATH, ConflictReason::GeneratedEdited).rule_id,
            RR0202_SYMBOLS_INVALID
        );
        assert_eq!(
            conflict_diagnostic(IGNORE_PATH, ConflictReason::ManagedIgnore).rule_id,
            RR0604_LOCAL_IGNORE_INVALID
        );
    }

    #[test]
    fn no_reserved_rule_is_in_the_emitted_table() {
        for reserved in RESERVED_RULES {
            assert!(
                !EMITTED_RULES.contains(&reserved),
                "{reserved} is reserved and is in EMITTED_RULES"
            );
        }
        for emitted in EMITTED_RULES {
            assert!(
                emitted.starts_with("RR0") && emitted.len() > 7,
                "{emitted} is not a numbered rule id"
            );
        }
    }

    /// The one thing about the `RR05xx` family this module may assume: those ids
    /// belong to `quality` and none of them is declared twice.
    #[test]
    fn the_quality_family_does_not_overlap_this_one() {
        for adjudicable in quality::ADJUDICABLE_RULES {
            assert!(!EMITTED_RULES.contains(&adjudicable));
            assert!(!RESERVED_RULES.contains(&adjudicable));
        }
    }

    #[test]
    fn every_status_spelling_is_the_serde_name() {
        for status in [
            CheckStatus::Ok,
            CheckStatus::Warnings,
            CheckStatus::Errors,
            CheckStatus::SnapshotMissing,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, format!("\"{}\"", status.as_str()));
        }
        for severity in [Severity::Fatal, Severity::Error, Severity::Warning] {
            let json = serde_json::to_string(&severity).unwrap();
            assert_eq!(json, format!("\"{}\"", severity.as_str()));
        }
    }

    /// D6's precedence, asserted rather than read off the source.
    #[test]
    fn the_exit_codes_are_zero_one_three_four_and_never_two() {
        assert_eq!(CheckStatus::Ok.exit_code(), 0);
        assert_eq!(CheckStatus::Warnings.exit_code(), 1);
        assert_eq!(CheckStatus::Errors.exit_code(), 3);
        assert_eq!(CheckStatus::SnapshotMissing.exit_code(), 4);
        for status in [
            CheckStatus::Ok,
            CheckStatus::Warnings,
            CheckStatus::Errors,
            CheckStatus::SnapshotMissing,
        ] {
            assert_ne!(status.exit_code(), 2, "{status:?} collides with clap's 2");
        }
    }

    #[test]
    fn severity_precedence_runs_fatal_error_warning_ok() {
        let counts = |fatal, errors, warnings| {
            CheckCounts {
                fatal,
                errors,
                warnings,
            }
            .status()
        };
        assert_eq!(counts(0, 0, 0), CheckStatus::Ok);
        assert_eq!(counts(0, 0, 7), CheckStatus::Warnings);
        assert_eq!(counts(0, 1, 7), CheckStatus::Errors);
        assert_eq!(counts(1, 1, 7), CheckStatus::SnapshotMissing);
    }

    /// A total order, so the printed list cannot depend on the order the rules
    /// happened to run in.
    #[test]
    fn the_diagnostic_sort_is_total_and_stable() {
        let mut first = vec![
            cache_corrupt("src/b.rs"),
            snapshot_missing(),
            cache_corrupt("src/a.rs"),
            conflict_diagnostic(SYMBOLS_PATH, ConflictReason::Symlink),
        ];
        let mut second = vec![
            conflict_diagnostic(SYMBOLS_PATH, ConflictReason::Symlink),
            cache_corrupt("src/a.rs"),
            cache_corrupt("src/b.rs"),
            snapshot_missing(),
        ];
        first.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        second.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));

        assert_eq!(first, second);
        assert_eq!(first[0].severity, Severity::Fatal);
        assert_eq!(first[1].severity, Severity::Error);
    }

    #[test]
    fn every_rebuild_reason_is_an_error_under_one_of_two_rules() {
        let reasons = [
            RebuildReason::BadMagic,
            RebuildReason::LengthMismatch,
            RebuildReason::ChecksumMismatch,
            RebuildReason::InvalidPayload,
            RebuildReason::TrailingBytes,
            RebuildReason::InvalidInvariant,
            RebuildReason::UnsupportedVersion { found: 1 },
            RebuildReason::BuildVersionMismatch { found: 1 },
            RebuildReason::RankingProfileMismatch { found: 1 },
            RebuildReason::LexicalMismatch,
        ];
        for reason in &reasons {
            let diagnostic = unusable_snapshot(reason);
            assert_eq!(diagnostic.severity, Severity::Error, "{reason:?}");
            assert!(
                diagnostic.rule_id == RR0002_SNAPSHOT_CORRUPT
                    || diagnostic.rule_id == RR0003_SNAPSHOT_INCOMPATIBLE,
                "{reason:?} produced {}",
                diagnostic.rule_id
            );
        }
    }

    /// The two renderers read one value, so they cannot disagree about it.
    #[test]
    fn text_and_json_agree_on_status_and_counts() {
        let diagnostics = vec![snapshot_missing(), cache_corrupt("src/a.rs")];
        let counts = counts_of(&diagnostics);
        let result = CheckResultV1 {
            schema_version: CHECK_SCHEMA_VERSION,
            status: counts.status(),
            counts,
            diagnostics,
            quality: None,
        };

        let text = render_check_text(&result);
        let json: serde_json::Value =
            serde_json::from_str(&render_check_json(&result).unwrap()).unwrap();

        assert!(text.starts_with("check: snapshot-missing · fatal: 1 · errors: 0 · warnings: 1\n"));
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["command"], CHECK_COMMAND);
        assert_eq!(json["status"], "snapshot-missing");
        assert_eq!(json["counts"]["fatal"], 1);
        assert_eq!(json["counts"]["warnings"], 1);
        assert_eq!(json["diagnostics"].as_array().unwrap().len(), 2);
        assert!(json.get("quality").is_none());
    }

    /// `schema_version` first, per `json_contract`: a consumer must be able to
    /// refuse a document before it interprets any of it.
    #[test]
    fn schema_version_is_the_first_key() {
        let result = CheckResultV1 {
            schema_version: CHECK_SCHEMA_VERSION,
            status: CheckStatus::Ok,
            counts: CheckCounts::default(),
            diagnostics: Vec::new(),
            quality: None,
        };

        assert!(render_check_json(&result)
            .unwrap()
            .starts_with("{\"schema_version\":1,\"command\":\"check\","));
    }
}

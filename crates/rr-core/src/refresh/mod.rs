//! Refresh vocabulary: what a refresh may do, why it fell back, and what it did.
//!
//! This module owns the *decisions* of an incremental refresh — the discovery
//! identity that decides whether a delta is meaningful at all, and the validated
//! plan that turns raw Git status items into dispositions. It deliberately owns
//! no I/O: the coordinator in `rr-git` observes the repository and executes the
//! plan, so the rules here can be tested without a repository at all, and
//! `rr-core` never needs to know how Git reports a status.

mod report;

use std::collections::BTreeMap;

use crate::lang::Lang;
use crate::path::RelPath;
use crate::walk::{WalkCfg, DEFAULT_EXCLUDES};

pub use report::{
    render_refresh_json, render_refresh_text, render_status_json, render_status_text, GitLabel,
    RefreshCommand, RefreshReport, ReportedMode, SnapshotLabel, StatusReport,
    REPORT_SCHEMA_VERSION,
};

/// How much of the repository a refresh is allowed to skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    /// Plan from the Git delta and retain everything the delta proves unchanged.
    Incremental,
    /// Rebuild from a full discovery, ignoring any delta.
    Full,
}

/// Whether a refresh published anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefreshOutcome {
    /// The snapshot on disk already described the repository.
    #[default]
    Unchanged,
    /// A new snapshot was published.
    Updated,
}

/// Why an incremental refresh had to rebuild everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FullReason {
    /// No snapshot has been published yet.
    MissingSnapshot,
    /// A snapshot exists but this binary cannot interpret it.
    IncompatibleSnapshot,
    /// A snapshot exists but does not survive strict validation.
    CorruptSnapshot,
    /// `HEAD` moved away from the commit the snapshot was built against.
    HeadChanged,
    /// Membership or byte-representation rules changed, so the delta is not
    /// about the same corpus.
    DiscoveryRulesChanged,
    /// The Git state could not be observed, including when there is no Git
    /// repository to observe.
    GitStatusUnavailable,
}

impl FullReason {
    /// The published spelling, identical to the serde name.
    ///
    /// Text output cannot go through serde, so the two spellings are pinned
    /// together by a test rather than by construction.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingSnapshot => "missing-snapshot",
            Self::IncompatibleSnapshot => "incompatible-snapshot",
            Self::CorruptSnapshot => "corrupt-snapshot",
            Self::HeadChanged => "head-changed",
            Self::DiscoveryRulesChanged => "discovery-rules-changed",
            Self::GitStatusUnavailable => "git-status-unavailable",
        }
    }

    /// A short human phrase for the one-line summary.
    #[must_use]
    pub const fn as_text(self) -> &'static str {
        match self {
            Self::MissingSnapshot => "no snapshot",
            Self::IncompatibleSnapshot => "incompatible snapshot",
            Self::CorruptSnapshot => "corrupt snapshot",
            Self::HeadChanged => "HEAD changed",
            Self::DiscoveryRulesChanged => "discovery rules changed",
            Self::GitStatusUnavailable => "git status unavailable",
        }
    }
}

/// Errors a refresh can fail with.
///
/// Every variant names the thing that went wrong; there is deliberately no
/// free-form variant, because a caller that cannot match on a failure cannot
/// react to it either.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// Another map or refresh holds the publication resource.
    #[error("another refresh is publishing to {path}")]
    PublicationLocked {
        /// The lock resource that was already held.
        path: std::path::PathBuf,
    },
    /// The repository state could not be observed or interpreted.
    #[error("git state unavailable: {source}")]
    GitState {
        /// The underlying failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Two status items claimed contradictory things about one path.
    #[error("invalid refresh plan for {path}: {reason}")]
    InvalidRefreshPlan {
        /// The path the contradiction is about.
        path: RelPath,
        /// What the contradiction was.
        reason: &'static str,
    },
    /// The repository moved under the refresh before it could publish.
    #[error("the repository changed during refresh; nothing was published")]
    RepositoryChanged,
    /// Cancellation was requested before the commit point.
    #[error("refresh cancelled before publication")]
    Cancelled,
}

// --- discovery identity -----------------------------------------------------

/// A digest over everything that decides *which* files are indexed and *how*
/// their bytes are read.
///
/// A snapshot records the digest it was built under. When the digest changes,
/// the recorded delta describes a different corpus than the one being rebuilt,
/// so no incremental plan derived from it can be trusted: one edited
/// `.gitignore` line can add or drop thousands of paths that Git status will
/// never mention.
pub struct DiscoveryIdentity {
    hasher: blake3::Hasher,
}

impl DiscoveryIdentity {
    /// Seeds the digest with this binary's discovery, extraction, and index
    /// rules together with the caller's traversal configuration.
    ///
    /// `threads` is deliberately absent: it changes how fast discovery runs,
    /// never which files it returns.
    #[must_use]
    pub fn new(walk: &WalkCfg) -> Self {
        let mut identity = Self {
            hasher: blake3::Hasher::new(),
        };
        identity.field("domain", b"rr.discovery.v1");

        identity.field(
            "default-excludes",
            DEFAULT_EXCLUDES.join("\u{1f}").as_bytes(),
        );
        identity.field("use-default-excludes", flag(walk.use_default_excludes));
        identity.field("standard-filters", flag(walk.standard_filters));
        identity.field("follow-symlinks", flag(walk.follow_symlinks));
        identity.field("detect-generated", flag(walk.detect_generated));
        identity.field(
            "custom-excludes",
            walk.custom_excludes.join("\u{1f}").as_bytes(),
        );
        identity.field(
            "languages",
            languages_key(walk.languages.as_deref()).as_bytes(),
        );
        identity.number("max-files", walk.max_files.map_or(u64::MAX, as_u64));

        identity.number("build-version", u64::from(crate::index::BUILD_VERSION));
        identity.number(
            "extractor-version",
            u64::from(crate::parser::EXTRACTOR_VERSION),
        );
        identity.number("fact-schema", u64::from(crate::facts::FACT_SCHEMA_VERSION));
        identity.number("lexical-version", u64::from(crate::lex::LEXICAL_VERSION));
        identity.number(
            "ranking-profile",
            u64::from(crate::ranking::RANKING_PROFILE_VERSION),
        );
        identity.number(
            "snapshot-schema",
            u64::from(crate::snapshot::SNAPSHOT_SCHEMA_VERSION),
        );
        identity
    }

    /// Mixes in a rule file that lives outside the worktree, and therefore
    /// outside every Git status the coordinator can observe.
    ///
    /// `None` records the file's *absence*, so creating it later changes the
    /// digest just as editing it would.
    pub fn mix_rule_file(&mut self, label: &str, contents: Option<&[u8]>) {
        match contents {
            Some(bytes) => self.field(label, bytes),
            None => self.field(label, b"\x00absent"),
        }
    }

    /// Finishes the digest.
    #[must_use]
    pub fn finish(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }

    /// Appends one length-prefixed labelled field.
    ///
    /// Length prefixing keeps two adjacent fields from being confused with one
    /// longer field whose contents happen to concatenate the same way.
    fn field(&mut self, label: &str, value: &[u8]) {
        self.hasher.update(&as_u64(label.len()).to_le_bytes());
        self.hasher.update(label.as_bytes());
        self.hasher.update(&as_u64(value.len()).to_le_bytes());
        self.hasher.update(value);
    }

    /// Appends one labelled scalar in a fixed-width little-endian encoding.
    fn number(&mut self, label: &str, value: u64) {
        self.field(label, &value.to_le_bytes());
    }
}

/// Widens a length for hashing. Saturating rather than wrapping, so a value
/// that could not be represented can never collide with a small one.
fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

const fn flag(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

/// Renders a language allowlist so that two allowlists differing only in order
/// or repetition produce the same key — `discover` treats them as the same rule.
fn languages_key(languages: Option<&[Lang]>) -> String {
    let Some(languages) = languages else {
        return "*".to_owned();
    };
    let mut names: Vec<&'static str> = languages.iter().map(Lang::as_str).collect();
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

// --- plan -------------------------------------------------------------------

/// What a refresh must do with one path whose state the delta reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// The path may still be part of the corpus; evaluate its current entry.
    Recheck,
    /// The path is gone from the worktree; drop whatever the snapshot recorded.
    Remove,
}

/// Raw dispositions collected from a Git status observation, before validation.
///
/// The draft exists so the coordinator can translate status items one at a time
/// without caring about ordering, duplication, or how a rename decomposes.
#[derive(Debug, Default)]
pub struct PlanDraft {
    dispositions: BTreeMap<RelPath, Disposition>,
    renames: BTreeMap<RelPath, RelPath>,
    conflicted: Vec<RelPath>,
    failure: Option<RefreshError>,
}

impl PlanDraft {
    /// Creates an empty draft.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `path` must be evaluated against its current worktree entry.
    pub fn recheck(&mut self, path: RelPath) {
        // Recheck outranks Remove on purpose. Two status items can legitimately
        // disagree — a staged deletion whose path was then recreated as an
        // untracked file reports both — and in that pair the worktree entry is
        // the one that still exists, so it decides.
        self.dispositions.insert(path, Disposition::Recheck);
    }

    /// Records that `path` no longer has a worktree entry.
    pub fn remove(&mut self, path: RelPath) {
        self.dispositions.entry(path).or_insert(Disposition::Remove);
    }

    /// Records that `source` moved to `target`.
    ///
    /// A rename is stored for reporting and decomposed for correctness: the old
    /// record goes away and the new path is evaluated from scratch, which is
    /// exactly what an add/delete pair would have produced. Rename detection can
    /// therefore be switched off without changing the published bytes.
    pub fn rename(&mut self, source: RelPath, target: RelPath) {
        if source == target {
            self.fail(target, "rename source equals its target");
            return;
        }
        if let Some(existing) = self.renames.insert(target.clone(), source.clone()) {
            if existing != source {
                self.fail(target, "two rename sources claim the same target");
                return;
            }
        }
        self.remove(source);
        self.recheck(target);
    }

    /// Records that `target` was copied from an untouched source.
    pub fn copy(&mut self, target: RelPath) {
        self.recheck(target);
    }

    /// Records that `path` has unmerged index stages.
    ///
    /// A conflicted path is always rechecked: no index stage can stand in for
    /// its content, so its current worktree entry is the only usable evidence.
    pub fn conflict(&mut self, path: RelPath) {
        self.conflicted.push(path.clone());
        self.recheck(path);
    }

    /// Validates and freezes the draft.
    ///
    /// # Errors
    /// Returns [`RefreshError::InvalidRefreshPlan`] when two status items made
    /// claims about one path that cannot both be true.
    pub fn build(mut self) -> Result<RefreshPlan, RefreshError> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }

        // A `BTreeMap` iterates in key order, which is exactly the sorted,
        // duplicate-free order both output vectors promise.
        let mut recheck = Vec::new();
        let mut remove = Vec::new();
        for (path, disposition) in self.dispositions {
            match disposition {
                Disposition::Recheck => recheck.push(path),
                Disposition::Remove => remove.push(path),
            }
        }

        let mut renames: Vec<(RelPath, RelPath)> = self
            .renames
            .into_iter()
            .map(|(target, source)| (source, target))
            .collect();
        renames.sort();

        // A source that ends up rechecked is not a contradiction: `git mv a b`
        // followed by a new untracked `a` reports both a move away from `a` and
        // a worktree entry at `a`, and both are true. One file moving to two
        // places is the claim that cannot be true.
        if let Some(window) = renames.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(RefreshError::InvalidRefreshPlan {
                path: window[0].0.clone(),
                reason: "one path is renamed to two targets",
            });
        }

        self.conflicted.sort();
        self.conflicted.dedup();

        Ok(RefreshPlan {
            mode: RefreshMode::Incremental,
            reason: None,
            recheck,
            remove,
            renames,
            conflicted: self.conflicted,
        })
    }

    /// Latches the first contradiction so the caller sees the earliest cause
    /// rather than whichever one happened to be validated last.
    fn fail(&mut self, path: RelPath, reason: &'static str) {
        if self.failure.is_none() {
            self.failure = Some(RefreshError::InvalidRefreshPlan { path, reason });
        }
    }
}

/// A validated decision about what this refresh will read and drop.
///
/// Fields are private and the only incremental constructor is
/// [`PlanDraft::build`], so a plan that violates its invariants cannot be
/// built: paths are sorted and unique, `recheck` and `remove` are disjoint, and
/// every rename has distinct endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshPlan {
    mode: RefreshMode,
    reason: Option<FullReason>,
    recheck: Vec<RelPath>,
    remove: Vec<RelPath>,
    renames: Vec<(RelPath, RelPath)>,
    conflicted: Vec<RelPath>,
}

impl RefreshPlan {
    /// A plan that rebuilds everything, for the stated reason.
    #[must_use]
    pub fn full(reason: FullReason) -> Self {
        Self {
            mode: RefreshMode::Full,
            reason: Some(reason),
            recheck: Vec::new(),
            remove: Vec::new(),
            renames: Vec::new(),
            conflicted: Vec::new(),
        }
    }

    /// A plan that rebuilds everything because the caller asked it to.
    #[must_use]
    pub fn requested_full() -> Self {
        Self {
            mode: RefreshMode::Full,
            reason: None,
            recheck: Vec::new(),
            remove: Vec::new(),
            renames: Vec::new(),
            conflicted: Vec::new(),
        }
    }

    /// Whether this plan retains anything at all.
    #[must_use]
    pub const fn mode(&self) -> RefreshMode {
        self.mode
    }

    /// Why a full rebuild was selected, when it was not requested outright.
    #[must_use]
    pub const fn reason(&self) -> Option<FullReason> {
        self.reason
    }

    /// Paths whose current worktree entry must be evaluated, sorted and unique.
    #[must_use]
    pub fn recheck(&self) -> &[RelPath] {
        &self.recheck
    }

    /// Paths whose prior records must be dropped, sorted and unique.
    #[must_use]
    pub fn remove(&self) -> &[RelPath] {
        &self.remove
    }

    /// Detected `(source, target)` pairs, kept for reporting only.
    #[must_use]
    pub fn renames(&self) -> &[(RelPath, RelPath)] {
        &self.renames
    }

    /// Paths with unmerged index stages, sorted and unique.
    #[must_use]
    pub fn conflicted(&self) -> &[RelPath] {
        &self.conflicted
    }

    /// Whether `path` may keep the facts the previous snapshot recorded.
    ///
    /// Only paths the delta never mentioned qualify: everything else either
    /// changed or lost the evidence that it did not.
    #[must_use]
    pub fn may_retain(&self, path: &RelPath) -> bool {
        self.mode == RefreshMode::Incremental
            && self.recheck.binary_search(path).is_err()
            && self.remove.binary_search(path).is_err()
    }

    /// How many distinct paths this plan reconsiders.
    ///
    /// The two vectors partition one map, so they cannot name the same path
    /// twice. Renames and conflicts are already counted through it: a rename
    /// removes its source and rechecks its target, and a conflict rechecks its
    /// path, so neither can contribute a path that is missing from both.
    #[must_use]
    pub fn touched_len(&self) -> usize {
        self.recheck.len().saturating_add(self.remove.len())
    }

    /// Whether the delta was empty, which is the precondition for the no-op path.
    ///
    /// A full plan touches nothing *because it consults no delta*, not because
    /// there is nothing to do, which is why the mode is part of the question.
    #[must_use]
    pub fn is_empty_delta(&self) -> bool {
        self.mode == RefreshMode::Incremental && self.touched_len() == 0
    }
}

#[cfg(test)]
mod tests;

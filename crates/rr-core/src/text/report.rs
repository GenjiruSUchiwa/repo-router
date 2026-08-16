//! What one run's text-artifact pass did, and the three ways it is spelled.
//!
//! Lives beside what builds it rather than beside [`crate::refresh::RunReport`]
//! that carries it: every field here is a fact about a [`StagedText`], and a
//! refresh module that had to import half of `text` to hold this value would be
//! the coupling this split exists to avoid.

use std::fmt::Write as _;

use super::{ArtifactKind, Conflict, StagedText};

/// What happened to `.rr/SYMBOLS.md`.
///
/// Four states rather than three-plus-`None`: the human clause has always
/// spelled the fourth one `absent`, and a JSON key that was `null` where the
/// text says `absent` is exactly the text/JSON divergence issue #43 exists to
/// remove.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolsState {
    /// No local index was written, because the run wrote nothing.
    #[default]
    Absent,
    /// Written because it was out of date.
    Written,
    /// Already correct.
    Unchanged,
    /// Rewritten because it was no longer valid, not merely out of date.
    Repaired,
}

impl SymbolsState {
    /// The published spelling, identical to the serde name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Repaired => "repaired",
        }
    }
}

/// The text-artifact half of one refresh.
///
/// Counts cover committed routers and overflow pages only. `.rr/SYMBOLS.md` has
/// its own state and `.rr/.gitignore` is not an artifact of this generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextReport {
    /// Committed artifacts written.
    pub written: u64,
    /// Committed artifacts already byte-identical.
    pub unchanged: u64,
    /// Committed artifacts unlinked.
    pub removed: u64,
    /// Routers whose purpose is still the generated placeholder.
    pub pending_purposes: u32,
    /// What happened to the local symbol index.
    pub symbols: SymbolsState,
    /// Paths written, in publication order.
    pub written_paths: Vec<String>,
    /// Paths unlinked.
    pub removed_paths: Vec<String>,
    /// Scopes no plan of which fits the map budget.
    ///
    /// #44 widened this from "one record is wider than the page": after it, a
    /// budget too small for the remainder lines themselves also lands here, and
    /// both mean the same thing to a reader — this scope could not be rendered
    /// truthfully. Do not restore the narrower wording.
    pub over_budget: Vec<String>,
    /// Every path that stopped the run, empty on a run that published.
    pub conflicts: Vec<Conflict>,
}

impl TextReport {
    /// Whether this pass changed anything on disk.
    ///
    /// `unchanged` is deliberately not consulted: an artifact that was already
    /// correct is the run finding nothing to do, which is the answer this
    /// predicate exists to give.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.written > 0
            || self.removed > 0
            || matches!(self.symbols, SymbolsState::Written | SymbolsState::Repaired)
    }

    /// The report for a generation that was already on disk in full.
    #[must_use]
    pub fn unchanged(staged: &StagedText) -> Self {
        let validation = staged.validation();
        Self {
            unchanged: u64::try_from(
                staged
                    .rendered()
                    .files()
                    .iter()
                    .filter(|file| file.kind() != ArtifactKind::Symbols)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            pending_purposes: validation.pending_purposes(),
            over_budget: validation.over_budget().to_vec(),
            symbols: SymbolsState::Unchanged,
            ..Self::default()
        }
    }

    /// The report for a run that refused before writing anything.
    ///
    /// Every action counter is zero, and stays zero even though some artifacts
    /// were already correct: these count what *this run* did, and it did
    /// nothing. What the caller acts on is `conflicts`.
    #[must_use]
    pub fn refused(staged: &StagedText) -> Self {
        let validation = staged.validation();
        Self {
            pending_purposes: validation.pending_purposes(),
            over_budget: validation.over_budget().to_vec(),
            conflicts: validation.conflicts().to_vec(),
            symbols: SymbolsState::Absent,
            ..Self::default()
        }
    }

    /// The one-line clause appended to the summary.
    #[must_use]
    pub fn clause(&self) -> String {
        format!(
            "; text: {} written, {} unchanged, {} removed, SYMBOLS.md {}, {} purpose pending",
            self.written,
            self.unchanged,
            self.removed,
            self.symbols.as_str(),
            self.pending_purposes
        )
    }

    /// The per-path breakdown behind that clause.
    #[must_use]
    pub fn verbose_lines(&self) -> String {
        let mut out = String::new();
        for path in &self.written_paths {
            let _ = writeln!(out, "  text write   {path}");
        }
        for path in &self.removed_paths {
            let _ = writeln!(out, "  text remove  {path}");
        }
        for scope in &self.over_budget {
            let _ = writeln!(
                out,
                "  text warning {scope}: no page of this scope fits the map budget"
            );
        }
        out.pop();
        out
    }

    /// The message a human sees on stderr when a run refuses.
    #[must_use]
    pub fn conflict_report(&self) -> String {
        let mut out = String::from("text artifacts conflict with the repository:\n");
        for conflict in &self.conflicts {
            let _ = writeln!(out, "  {conflict}");
        }
        out.push_str("\nnothing was written. Resolve each file, then run `rr map` again.");
        out
    }
}

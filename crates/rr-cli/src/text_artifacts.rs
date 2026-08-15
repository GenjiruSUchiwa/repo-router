//! Writing issue #11's committed maps and local symbol index.
//!
//! Staging decides everything before anything is written, so a repository with
//! a conflict is left exactly as it was found. Publication then happens under
//! the guard the snapshot was published with — `.rr/SYMBOLS.md` first, maps
//! deepest-first, and the root `MAP.md` last as the generation marker.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context;
use rr_core::index::Snapshot;
use rr_core::text::{
    apply_managed_block, stage_text_artifacts, ArtifactKind, Conflict, StagedText, IGNORE_PATH,
};

/// What happened to `.rr/SYMBOLS.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolsState {
    Written,
    Unchanged,
    Repaired,
}

impl SymbolsState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Repaired => "repaired",
        }
    }
}

/// The text clause of a refresh summary.
///
/// Counts cover committed routers and overflow pages only. `.rr/SYMBOLS.md` has
/// its own state and `.rr/.gitignore` is not an artifact of this generation.
#[derive(Debug, Clone, Default)]
pub struct TextReport {
    pub written: u64,
    pub unchanged: u64,
    pub removed: u64,
    pub pending_purposes: u32,
    pub symbols: Option<SymbolsState>,
    pub written_paths: Vec<String>,
    pub removed_paths: Vec<String>,
    pub over_budget: Vec<String>,
}

impl TextReport {
    /// The one-line clause appended to the summary.
    #[must_use]
    pub fn clause(&self) -> String {
        let symbols = self.symbols.map_or("absent", SymbolsState::as_str);
        format!(
            "; text: {} written, {} unchanged, {} removed, SYMBOLS.md {}, {} purpose pending",
            self.written, self.unchanged, self.removed, symbols, self.pending_purposes
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
                "  text warning {scope}: a single record exceeds the page budget"
            );
        }
        out.pop();
        out
    }
}

/// Everything decided, nothing written.
///
/// # Errors
/// Returns an error when the snapshot cannot be projected or the repository
/// cannot be read at all.
pub fn stage(snapshot: &Snapshot, root: &Path, budget: u32) -> anyhow::Result<StagedText> {
    stage_text_artifacts(snapshot, root, budget).context("stage text artifacts")
}

/// The report for a generation that was already on disk in full.
#[must_use]
pub fn unchanged(staged: &StagedText) -> TextReport {
    let validation = staged.validation();
    TextReport {
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
        symbols: Some(SymbolsState::Unchanged),
        ..TextReport::default()
    }
}

/// The message a caller prints before exiting non-zero.
#[must_use]
pub fn conflict_report(conflicts: &[Conflict]) -> String {
    let mut out = String::from("text artifacts conflict with the repository:\n");
    for conflict in conflicts {
        let _ = writeln!(out, "  {conflict}");
    }
    out.push_str("\nnothing was written. Resolve each file, then run `rr map` again.");
    out
}

/// Writes one generation, in the order a reader can rely on, and reads it back.
///
/// Verification is not a step a caller can skip, because the failure it catches
/// — a repository holding two generations at once — is invisible until someone
/// reads a map that names a snapshot nobody has.
///
/// # Errors
/// Returns I/O failures, and an error when the bytes on disk do not read back
/// as the generation that was just written.
pub fn publish(
    staged: &StagedText,
    snapshot: &Snapshot,
    root: &Path,
    budget: u32,
) -> anyhow::Result<TextReport> {
    let report = write_generation(staged, root)?;
    confirm(snapshot, root, budget)?;
    Ok(report)
}

fn write_generation(staged: &StagedText, root: &Path) -> anyhow::Result<TextReport> {
    let validation = staged.validation();
    let mut report = TextReport {
        pending_purposes: validation.pending_purposes(),
        over_budget: validation.over_budget().to_vec(),
        ..TextReport::default()
    };

    // `files()` is already in publication order: the local index first, then
    // maps deepest-first with the root last. A crash therefore leaves the root
    // map — the generation marker — either wholly old or wholly new.
    for file in staged.rendered().files() {
        let fresh = validation.fresh().iter().any(|path| path == file.path());
        let symbols = file.kind() == ArtifactKind::Symbols;

        if fresh {
            if symbols {
                report.symbols = Some(SymbolsState::Unchanged);
            } else {
                report.unchanged += 1;
            }
            continue;
        }

        write_file(root, file.path(), file.bytes())?;
        if symbols {
            report.symbols = Some(if validation.symbols_repaired() {
                SymbolsState::Repaired
            } else {
                SymbolsState::Written
            });
        } else {
            report.written += 1;
        }
        report.written_paths.push(file.path().to_owned());
    }

    // Removals last: a page is only unlinked once the generation that replaces
    // it is fully on disk.
    for path in validation.removable() {
        std::fs::remove_file(root.join(path))
            .with_context(|| format!("remove stale page {path}"))?;
        report.removed += 1;
        report.removed_paths.push(path.clone());
    }

    write_managed_ignore(root)?;
    Ok(report)
}

/// Reads the whole generation back and checks it says what was written.
fn confirm(snapshot: &Snapshot, root: &Path, budget: u32) -> anyhow::Result<()> {
    let after = stage(snapshot, root, budget)?;
    let validation = after.validation();
    anyhow::ensure!(
        validation.is_up_to_date(),
        "text artifacts did not survive publication: {} stale, {} missing, {} conflicting",
        validation.stale().len(),
        validation.missing().len(),
        validation.conflicts().len()
    );
    Ok(())
}

fn write_file(root: &Path, path: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let absolute = root.join(path);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create directory for {path}"))?;
    }
    std::fs::write(&absolute, bytes).with_context(|| format!("write {path}"))
}

/// Keeps `.rr/.gitignore` hiding the local artifacts, preserving other lines.
fn write_managed_ignore(root: &Path) -> anyhow::Result<()> {
    let absolute = root.join(IGNORE_PATH);
    let existing = match std::fs::read_to_string(&absolute) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read .rr/.gitignore"),
    };

    let wanted = apply_managed_block(existing.as_deref()).context("update .rr/.gitignore")?;
    if existing.as_deref() == Some(wanted.as_str()) {
        return Ok(());
    }
    write_file(root, IGNORE_PATH, wanted.as_bytes())
}

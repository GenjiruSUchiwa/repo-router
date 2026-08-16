//! Writing issue #11's committed maps and local symbol index.
//!
//! Staging decides everything before anything is written, so a repository with
//! a conflict is left exactly as it was found. Publication then happens under
//! the guard the snapshot was published with — `.rr/SYMBOLS.md` first, maps
//! deepest-first, and the root `MAP.md` last as the generation marker.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use rr_core::index::Snapshot;
use rr_core::text::{
    apply_managed_block, stage_text_artifacts, ArtifactKind, StagedText, SymbolsState, TextReport,
    IGNORE_PATH,
};

/// Everything decided, nothing written.
///
/// # Errors
/// Returns an error when the snapshot cannot be projected or the repository
/// cannot be read at all.
pub fn stage(snapshot: &Snapshot, root: &Path, budget: u32) -> anyhow::Result<StagedText> {
    stage_text_artifacts(snapshot, root, budget).context("stage text artifacts")
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

    // One membership set rather than a scan per file: the two lists are the
    // same length, so asking the question linearly costs a comparison per pair
    // of artifacts in the repository.
    let fresh: BTreeSet<&str> = validation.fresh().iter().map(String::as_str).collect();

    // `files()` is already in publication order: the local index first, then
    // maps deepest-first with the root last. A crash therefore leaves the root
    // map — the generation marker — either wholly old or wholly new.
    for file in staged.rendered().files() {
        let fresh = fresh.contains(file.path());
        let symbols = file.kind() == ArtifactKind::Symbols;

        if fresh {
            if symbols {
                report.symbols = SymbolsState::Unchanged;
            } else {
                report.unchanged += 1;
            }
            continue;
        }

        write_file(root, file.path(), file.bytes())?;
        if symbols {
            report.symbols = if validation.symbols_repaired() {
                SymbolsState::Repaired
            } else {
                SymbolsState::Written
            };
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
///
/// After the write rather than before it, and deliberately. Rendering twice and
/// comparing would catch a render that disagrees with itself; reading the files
/// back catches that *and* a write that did not land, a concurrent editor, and
/// a filesystem that lied — for the cost of one extra stage on a path that has
/// just done I/O for every artifact in the repository.
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

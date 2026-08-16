//! `rr init`: install the contract that tells an agent how to navigate here.
//!
//! Everything this writes is either a marked region inside a file the user owns
//! or a whole file rr stamps. That split is the whole design: a region has
//! self-evident boundaries, so re-applying it is safe by construction; a whole
//! file has none, so it carries a stamp and is refused when the stamp does not
//! match. Nothing here overwrites bytes it cannot prove it wrote.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Args as ClapArgs;
use rr_core::agent::{self, CONTRACT_BLOCK, CONTRACT_MARKERS};
use rr_core::text::{
    apply_block, apply_managed_block, DUPLICATE_MARKERS_REASON, IGNORE_PATH,
    MALFORMED_MARKERS_REASON,
};
use rr_core::workspace;

use crate::output::Output;
use crate::refresh::exit;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Repository to initialize. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Emit the report as one JSON object instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

/// What happened to one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Created,
    Updated,
    Unchanged,
    Refused(Refusal),
}

impl Outcome {
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
            Self::Refused(_) => "refused",
        }
    }
}

/// Why a target was left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// Two begin or two end markers — usually a merge that kept both sides.
    MarkersDuplicated,
    /// A marker is missing or the end precedes the begin.
    MarkersMalformed,
    /// A file at an rr path that rr did not write, or that has been edited.
    NotOurs,
    /// Content this crate cannot read back: not UTF-8, or mixed newlines.
    Unreadable,
    /// The read itself failed: a directory, a permission, a broken device.
    ReadFailed,
    /// The write itself failed.
    WriteFailed,
}

impl Refusal {
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::MarkersDuplicated => {
                "the rr markers appear more than once; \
                resolve the duplicate region and run `rr init` again"
            }
            Self::MarkersMalformed => {
                "the rr markers are unpaired or inverted; \
                fix or delete the region and run `rr init` again"
            }
            Self::NotOurs => {
                "this file was not written by rr; \
                delete it if you want rr's version"
            }
            Self::Unreadable => "this file is not UTF-8 or has mixed line endings",
            Self::ReadFailed => "the file could not be read",
            Self::WriteFailed => "the file could not be written",
        }
    }
}

struct Report {
    targets: Vec<(String, Outcome)>,
    /// Whether the rr skill itself was installed for the first time.
    ///
    /// The restart note exists because Claude Code discovers a *skill* at
    /// startup, so this asks about the skill and not about `.claude/skills/`:
    /// a repository that already keeps another skill there still gets a brand
    /// new `rr` one, and still has to restart to see it.
    skill_created: bool,
}

impl Report {
    /// `0` unless something was refused.
    ///
    /// "Wrote nothing" is success, not a distinct code: a user running
    /// `rr init` twice asked for the contract to be installed, and it is.
    #[must_use]
    fn exit_code(&self) -> u8 {
        if self
            .targets
            .iter()
            .any(|(_, outcome)| matches!(outcome, Outcome::Refused(_)))
        {
            exit::ERROR
        } else {
            exit::OK
        }
    }
}

/// Runs `rr init` and returns its exit code.
///
/// # Errors
/// Returns only failures that stop the run before it can report anything —
/// resolving the working directory, or creating `.rr`. Everything a target can
/// do wrong is an [`Outcome`], because a user with one hand-written file should
/// still get the other three.
pub fn run(args: &Args) -> anyhow::Result<u8> {
    let root = match args.root.clone() {
        Some(root) => root,
        None => std::env::current_dir().context("resolve current directory")?,
    };

    // Sampled before `ensure_private` stamps the file, because that write is the
    // only reason a `.rr/.gitignore` this run created reads back as pre-existing
    // — and reporting `updated` for a file rr made moments ago is a report of
    // somebody else's edit that never happened.
    let ignore_existed = root.join(IGNORE_PATH).exists();

    // First, and before anything else is written: the state directory has to be
    // marked private before it holds anything, which is the same order
    // `RepositoryWriteGuard::acquire` uses (`rr-git/src/guard.rs:38-58`).
    workspace::ensure_private(&root).context("create .rr")?;

    let mut targets = Vec::with_capacity(agent::AGENT_FILES.len() + 2);
    let (ignore_path, mut ignore_outcome) = apply_region(&root, IGNORE_PATH, apply_managed_block);
    if !ignore_existed && ignore_outcome == Outcome::Updated {
        ignore_outcome = Outcome::Created;
    }
    targets.push((ignore_path, ignore_outcome));
    for name in agent::AGENT_FILES {
        targets.push(apply_region(&root, name, |existing| {
            apply_block(existing, CONTRACT_MARKERS, CONTRACT_BLOCK)
        }));
    }
    let (skill_path, skill_outcome) = install_skill(&root);
    let skill_created = skill_outcome == Outcome::Created;
    targets.push((skill_path, skill_outcome));
    let report = Report {
        targets,
        skill_created,
    };

    if args.json {
        Output::print_text(&render_json(&report))?;
    } else {
        Output::print_text(&render_text(&report))?;
    }
    Ok(report.exit_code())
}

/// Applies one marked region to one file, creating it if it is absent.
///
/// The read is case-insensitive about the file *name* and the write reuses
/// whatever name it found. On a case-insensitive filesystem the two spellings
/// are one file and this changes nothing; on a case-sensitive one it is what
/// stops `rr init` installing the contract into a second `CLAUDE.md` beside a
/// repository's existing `claude.md`. See D8.
///
/// Returns the path as it was actually written, not the one that was asked for,
/// so the report names a file the reader can open.
fn apply_region<F>(root: &Path, name: &str, wanted: F) -> (String, Outcome)
where
    // Spelled out, not `TextResult`: that alias is private to `rr-core`
    // (`text/mod.rs:181` has no `pub`), while `TextError` at `text/mod.rs:152`
    // is exported. Same type, one of the two names.
    F: FnOnce(Option<&str>) -> Result<String, rr_core::text::TextError>,
{
    let path = resolve_existing_name(root, name);
    let reported = reported_name(name, &path);
    let refused = |reason| (reported.clone(), Outcome::Refused(reason));

    let existing = match std::fs::read(&path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(_) => return refused(Refusal::Unreadable),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return refused(Refusal::ReadFailed),
    };

    let content = match wanted(existing.as_deref()) {
        Ok(content) => content,
        // Compared by identity, not by substring: `TextError`'s own
        // documentation avoids free-form strings "so that a message change can
        // never become a behavior change", and a `contains` here would put that
        // guarantee back at the mercy of the wording.
        Err(rr_core::text::TextError::ManagedIgnore { reason })
            if reason == DUPLICATE_MARKERS_REASON =>
        {
            return refused(Refusal::MarkersDuplicated)
        }
        Err(rr_core::text::TextError::ManagedIgnore { reason })
            if reason == MALFORMED_MARKERS_REASON =>
        {
            return refused(Refusal::MarkersMalformed)
        }
        Err(_) => return refused(Refusal::Unreadable),
    };

    // D9: bytes, not mtime. A second run must leave every file untouched, and
    // "untouched" has to mean the write did not happen, not that it happened to
    // produce the same content.
    if existing.as_deref() == Some(content.as_str()) {
        return (reported, Outcome::Unchanged);
    }
    let created = existing.is_none();
    if write_atomic(&path, content.as_bytes()).is_err() {
        return refused(Refusal::WriteFailed);
    }
    let outcome = if created {
        Outcome::Created
    } else {
        Outcome::Updated
    };
    (reported, outcome)
}

/// The target's own name, respelled to match the file that was found.
///
/// Only the last component can differ — that is all [`resolve_existing_name`]
/// looks at — so only the last component is substituted, which keeps the
/// separator in the reported path a `/` on every platform.
fn reported_name(name: &str, path: &Path) -> String {
    let Some(found) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return name.to_owned();
    };
    match name.rfind('/') {
        Some(cut) => format!("{}{found}", &name[..=cut]),
        None => found.to_owned(),
    }
}

/// The path to use for a target, honouring an existing file's own spelling.
///
/// Only the last component is compared, and only when the parent can be listed.
/// A repository with both `CLAUDE.md` and `claude.md` on a case-sensitive
/// filesystem is a repository that already has a problem; this picks the exact
/// match when there is one and does not try to merge them.
fn resolve_existing_name(root: &Path, name: &str) -> PathBuf {
    let exact = root.join(name);
    if exact.exists() {
        return exact;
    }
    let (Some(parent), Some(file_name)) = (exact.parent(), exact.file_name()) else {
        return exact;
    };
    let Some(file_name) = file_name.to_str() else {
        return exact;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return exact;
    };
    for entry in entries.flatten() {
        if let Ok(found) = entry.file_name().into_string() {
            if found.eq_ignore_ascii_case(file_name) {
                return parent.join(found);
            }
        }
    }
    exact
}

/// Installs `SKILL.md`, refusing anything rr did not write.
///
/// [`Outcome::Created`] is what the restart note keys on, so a refusal or an
/// unchanged file never prints one: both mean this run installed no new skill
/// for Claude Code to discover.
fn install_skill(root: &Path) -> (String, Outcome) {
    let path = resolve_existing_name(root, agent::SKILL_PATH);
    let reported = reported_name(agent::SKILL_PATH, &path);
    let refused = |reason| (reported.clone(), Outcome::Refused(reason));
    let wanted = agent::skill_document();

    let existing = match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes == wanted.as_bytes() {
                return (reported, Outcome::Unchanged);
            }
            match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(_) => return refused(Refusal::Unreadable),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return refused(Refusal::ReadFailed),
    };

    if let Some(existing) = existing.as_deref() {
        if !agent::is_rr_written_skill(existing) {
            return refused(Refusal::NotOurs);
        }
    }

    if std::fs::create_dir_all(root.join(agent::SKILL_DIR)).is_err() {
        return refused(Refusal::WriteFailed);
    }
    if write_atomic(&path, wanted.as_bytes()).is_err() {
        return refused(Refusal::WriteFailed);
    }
    let outcome = if existing.is_none() {
        Outcome::Created
    } else {
        Outcome::Updated
    };
    (reported, outcome)
}

/// Replaces a file's contents without ever leaving a half-written one behind.
///
/// [`std::fs::write`] truncates in place, so a crash between the truncate and
/// the last byte leaves `AGENTS.md` or `CLAUDE.md` short — and most of what is
/// in those files is prose rr does not own and cannot regenerate. This is the
/// pattern `rr-core` already uses for the snapshot (`snapshot.rs:174`) and the
/// fact cache (`cache.rs:212`): a unique temp file in the target's own
/// directory, then a rename that either happens or does not.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(contents)?;
    carry_mode(path, &temp)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Gives the replacement the mode the original had.
///
/// A temp file is created `0600`. These targets are files a repository commits
/// and a human reads, so replacing one must not quietly narrow its permissions;
/// a target that does not exist yet takes the mode a plain create would give.
#[cfg(unix)]
fn carry_mode(path: &Path, temp: &tempfile::NamedTempFile) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = std::fs::metadata(path).map_or(0o644, |meta| meta.permissions().mode() & 0o777);
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn carry_mode(_path: &Path, _temp: &tempfile::NamedTempFile) -> std::io::Result<()> {
    Ok(())
}

fn render_json(report: &Report) -> String {
    let targets: Vec<agent::InitTarget<'_>> = report
        .targets
        .iter()
        .map(|(path, outcome)| agent::InitTarget {
            path: path.as_str(),
            outcome: outcome.as_str(),
            reason: match outcome {
                Outcome::Refused(reason) => Some(reason.as_str()),
                _ => None,
            },
        })
        .collect();
    agent::render_init_json(&targets)
}

fn render_text(report: &Report) -> String {
    let (mut created, mut updated, mut unchanged, mut refused) = (0usize, 0, 0, 0);
    for (_, outcome) in &report.targets {
        match outcome {
            Outcome::Created => created += 1,
            Outcome::Updated => updated += 1,
            Outcome::Unchanged => unchanged += 1,
            Outcome::Refused(_) => refused += 1,
        }
    }

    let mut counts = Vec::new();
    if created > 0 {
        counts.push(format!("{created} created"));
    }
    if updated > 0 {
        counts.push(format!("{updated} updated"));
    }
    if unchanged > 0 {
        counts.push(format!("{unchanged} unchanged"));
    }
    if refused > 0 {
        counts.push(format!("{refused} refused"));
    }

    let mut out = format!(
        "rr init: {} targets — {}",
        report.targets.len(),
        counts.join(", ")
    );
    for (path, outcome) in &report.targets {
        let label = outcome.as_str();
        match outcome {
            Outcome::Refused(reason) => {
                let _ = write!(out, "\n  {label:<10} {path} — {}", reason.as_str());
            }
            _ => {
                let _ = write!(out, "\n  {label:<10} {path}");
            }
        }
    }
    out.push_str("\n  next: rr map");
    // Advice about committing the contract is advice about files that exist. A
    // run that refused every target installed nothing, and telling that user to
    // commit the contract points them at a file rr declined to write.
    if refused < report.targets.len() {
        out.push_str(
            "\n  note: AGENTS.md, CLAUDE.md and .claude/ carry the agent contract; commit them\n        if your team should share it.",
        );
    }
    if report.skill_created {
        out.push_str("\n  note: the rr skill is new; restart Claude Code so it picks it up.");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report has to name the file that was written. Only the last
    /// component can have been respelled, and the separator stays a `/` so the
    /// path is the one `rr` documents rather than the one this platform spells.
    #[test]
    fn a_respelled_file_is_reported_under_the_name_on_disk() {
        assert_eq!(
            reported_name("CLAUDE.md", Path::new("/repo/claude.md")),
            "claude.md"
        );
        assert_eq!(
            reported_name(
                ".claude/skills/rr/SKILL.md",
                Path::new("/repo/.claude/skills/rr/Skill.md")
            ),
            ".claude/skills/rr/Skill.md"
        );
        assert_eq!(
            reported_name(".rr/.gitignore", Path::new("/repo/.rr/.gitignore")),
            ".rr/.gitignore"
        );
    }
}

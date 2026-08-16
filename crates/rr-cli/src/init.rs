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
use rr_core::text::{apply_block, apply_managed_block, IGNORE_PATH};
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
            Self::WriteFailed => "the file could not be written",
        }
    }
}

struct Report {
    targets: Vec<(String, Outcome)>,
    skills_dir_created: bool,
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

    // First, and before anything else is written: the state directory has to be
    // marked private before it holds anything, which is the same order
    // `RepositoryWriteGuard::acquire` uses (`rr-git/src/guard.rs:38-58`).
    workspace::ensure_private(&root).context("create .rr")?;

    let mut report = Report {
        targets: Vec::new(),
        skills_dir_created: false,
    };
    report.targets.push((
        IGNORE_PATH.to_owned(),
        apply_region(&root, IGNORE_PATH, apply_managed_block),
    ));
    for name in agent::AGENT_FILES {
        report.targets.push((
            name.to_owned(),
            apply_region(&root, name, |existing| {
                apply_block(existing, CONTRACT_MARKERS, CONTRACT_BLOCK)
            }),
        ));
    }
    let (skill_outcome, skills_dir_created) = install_skill(&root);
    report
        .targets
        .push((agent::SKILL_PATH.to_owned(), skill_outcome));
    report.skills_dir_created = skills_dir_created;

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
fn apply_region<F>(root: &Path, name: &str, wanted: F) -> Outcome
where
    // Spelled out, not `TextResult`: that alias is private to `rr-core`
    // (`text/mod.rs:181` has no `pub`), while `TextError` at `text/mod.rs:152`
    // is exported. Same type, one of the two names.
    F: FnOnce(Option<&str>) -> Result<String, rr_core::text::TextError>,
{
    let path = resolve_existing_name(root, name);
    let existing = match std::fs::read(&path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(_) => return Outcome::Refused(Refusal::Unreadable),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Outcome::Refused(Refusal::Unreadable),
    };

    let content = match wanted(existing.as_deref()) {
        Ok(content) => content,
        Err(rr_core::text::TextError::ManagedIgnore { reason }) => {
            return Outcome::Refused(if reason.contains("more than once") {
                Refusal::MarkersDuplicated
            } else {
                Refusal::MarkersMalformed
            })
        }
        Err(_) => return Outcome::Refused(Refusal::Unreadable),
    };

    // D9: bytes, not mtime. A second run must leave every file untouched, and
    // "untouched" has to mean the write did not happen, not that it happened to
    // produce the same content.
    if existing.as_deref() == Some(content.as_str()) {
        return Outcome::Unchanged;
    }
    let created = existing.is_none();
    if write_file(&path, content.as_bytes()).is_err() {
        return Outcome::Refused(Refusal::WriteFailed);
    }
    if created {
        Outcome::Created
    } else {
        Outcome::Updated
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
/// The boolean is whether this run created `.claude/skills/` *and* the skill
/// write succeeded. Early refusals carry `false` so a restart note is never
/// printed for a file that was not installed.
fn install_skill(root: &Path) -> (Outcome, bool) {
    let path = resolve_existing_name(root, agent::SKILL_PATH);
    let wanted = agent::skill_document();

    let existing = match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes == wanted.as_bytes() {
                return (Outcome::Unchanged, false);
            }
            match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(_) => return (Outcome::Refused(Refusal::Unreadable), false),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return (Outcome::Refused(Refusal::Unreadable), false),
    };

    if let Some(existing) = existing.as_deref() {
        if !agent::is_rr_written_skill(existing) {
            return (Outcome::Refused(Refusal::NotOurs), false);
        }
    }

    // Amendment C: sampled *before* the directory is created, because that is
    // the only moment the answer is still knowable.
    let skills_dir_was_absent = !root.join(".claude/skills").exists();
    if std::fs::create_dir_all(root.join(agent::SKILL_DIR)).is_err() {
        return (Outcome::Refused(Refusal::WriteFailed), false);
    }
    if write_file(&path, wanted.as_bytes()).is_err() {
        return (Outcome::Refused(Refusal::WriteFailed), false);
    }
    let outcome = if existing.is_none() {
        Outcome::Created
    } else {
        Outcome::Updated
    };
    (outcome, skills_dir_was_absent)
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

fn render_json(report: &Report) -> String {
    let targets: Vec<(&str, &str, Option<&str>)> = report
        .targets
        .iter()
        .map(|(path, outcome)| match outcome {
            Outcome::Created => (path.as_str(), "created", None),
            Outcome::Updated => (path.as_str(), "updated", None),
            Outcome::Unchanged => (path.as_str(), "unchanged", None),
            Outcome::Refused(reason) => (path.as_str(), "refused", Some(reason.as_str())),
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
    out.push_str(
        "\n  note: AGENTS.md, CLAUDE.md and .claude/ carry the agent contract; commit them\n        if your team should share it.",
    );
    if report.skills_dir_created {
        out.push_str(
            "\n  note: .claude/skills/ is new; restart Claude Code so it picks up the skill.",
        );
    }
    out
}

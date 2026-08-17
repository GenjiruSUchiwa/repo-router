//! `rr check` at the command line.
//!
//! The command asks a repository every question in [`rr_core::check`], prints
//! the answers, and exits on the verdict. It writes nothing: see
//! [`rr_core::check::check`], whose read-only guarantee this command exists to
//! expose.
//!
//! # Why the exit code does not come through `main`'s `finish`
//!
//! Every other command routes its result through `finish`, which returns `1` on
//! failure. `check` cannot: `1` is already its verdict for "warnings only", and
//! a caller that read `1` as a failure would treat a stale map as a crash. So a
//! failed *invocation* leaves by `2` instead — the code `clap` returns for a
//! mistyped flag — because both mean the same thing to a script: the command
//! never got as far as an opinion about the repository. The three verdicts that
//! *are* opinions keep `0`, `1`, `3` and `4` to themselves.

use std::path::PathBuf;

use anyhow::Context;
use clap::Args as ClapArgs;
use rr_core::cancel::CancelToken;
use rr_core::check::{check, render_check_json, render_check_text};
use rr_core::refresh::SnapshotLabel;

use crate::output::Output;

/// The exit code for an invocation that never reached a verdict.
///
/// Shared with `clap` on purpose; the doc comment on
/// [`rr_core::check::CheckStatus::exit_code`] holds the argument.
pub const NOT_EVALUATED: u8 = 2;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Repository to check. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Also adjudicate a corpus quality report produced elsewhere.
    ///
    /// Opt-in, and additive: it contributes the report's blocking findings and
    /// suppresses none of the checks rr makes about the repository it is
    /// standing in.
    #[arg(long, value_name = "PATH")]
    pub quality_report: Option<PathBuf>,
    /// Emit the report as one JSON object instead of the human report.
    #[arg(long)]
    pub json: bool,
}

/// Runs `rr check` and returns its exit code.
///
/// The snapshot's freshness is asked of `rr_git` here rather than inside
/// [`check`], because that question needs a Git working tree and `rr-core` is
/// the crate `rr-git` depends on. A repository that cannot answer it — no Git
/// directory, an unborn `HEAD`, a comparison that failed — yields `None`, and
/// the staleness rule is then not evaluated at all: an unasked question is
/// reported as unasked rather than as a pass.
///
/// Only the label crosses over. The rest of the status report carries
/// `unresolved`, and no rule may be keyed on that count.
///
/// # Errors
/// Returns a failure only when the check could not be carried out. A broken
/// artifact is a diagnostic and an exit code, which is the command's entire
/// purpose.
pub fn run(args: &Args) -> anyhow::Result<u8> {
    let root = match args.root.clone() {
        Some(root) => root,
        None => std::env::current_dir().context("resolve current directory")?,
    };

    let cancel = CancelToken::new();
    let label = snapshot_label(&root, &cancel);
    let result =
        check(&root, label, args.quality_report.as_deref(), &cancel).context("check repository")?;

    if args.json {
        Output::print_text(&render_check_json(&result).context("render report as JSON")?)?;
    } else {
        Output::print_text(render_check_text(&result).trim_end())?;
    }
    Ok(result.exit_code())
}

/// The working tree's verdict on the snapshot, when it can be had.
///
/// Every failure collapses to `None` deliberately. This is not the question the
/// user asked — they asked for the repository's diagnostics — and turning "this
/// directory is not a Git repository" into a failed `rr check` would make the
/// command useless everywhere it is most useful: an exported tree, a container
/// with no `.git`, a vendored copy under review.
fn snapshot_label(root: &std::path::Path, cancel: &CancelToken) -> Option<SnapshotLabel> {
    rr_git::status(root, cancel)
        .ok()
        .map(|report| report.snapshot)
}

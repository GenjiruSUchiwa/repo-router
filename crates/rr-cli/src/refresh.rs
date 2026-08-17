//! `rr refresh`, `rr map`, and `rr status` at the command line.
//!
//! All three are one operation seen from three angles: `map` is `refresh
//! --full`, and `status` is a refresh that stops after deciding. They share the
//! report type and the renderers, so the human line and the JSON object cannot
//! disagree about what happened, and neither can `status` disagree with the
//! `refresh` it is describing.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Args;
use rr_core::cancel::CancelToken;
use rr_core::refresh::{
    render_refresh_json, render_refresh_text, render_refresh_verbose, render_status_json,
    render_status_text, RefreshCommand, RefreshMode, RefreshReport, ReportDetail, RunReport,
};
use rr_core::text::{StagedText, TextReport, DEFAULT_MAP_BUDGET};

use crate::output::Output;
use crate::text_artifacts;

/// Exit codes shared by the three commands.
///
/// Deliberately only three, and `2` is deliberately not among them. A command
/// that ran and reported a stale index did what it was asked; the answer is in
/// the report, not in whether the process succeeded. Spending an exit code on
/// it would collide with the `2` that `clap` already returns for a mistyped
/// flag, leaving a script unable to tell "your index is out of date" from "you
/// misspelled `--threads`".
///
/// A caller may also see 141 (`128 + SIGPIPE`). That is not a constant: the
/// process is killed, it does not return. `INTERRUPTED` is returned because
/// refresh catches the first SIGINT and stops at a boundary of its own
/// choosing. Every other terminating signal — SIGPIPE, a second Ctrl-C, a
/// `kill`, a closed terminal — ends the process instead, and the exit status is
/// the signal's rather than one of these. The handler in `main` releases the
/// publication claim on the way out; it does not make the death survivable.
pub mod exit {
    /// The command did what it was asked.
    pub const OK: u8 = 0;
    /// The command failed, and the previous snapshot is untouched.
    pub const ERROR: u8 = 1;
    /// The command was interrupted, spelled the way every shell spells it.
    pub const INTERRUPTED: u8 = 128 + 2;
}

#[derive(Args, Debug)]
pub struct RefreshArgs {
    /// Repository to refresh. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Parser threads.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub threads: Option<u16>,
    /// Print the per-stage counters as well as the summary.
    #[arg(long, short)]
    pub verbose: bool,
    /// Rebuild everything instead of consulting the Git delta.
    #[arg(long)]
    pub full: bool,
    /// Emit the report as one JSON object instead of a human line.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Repository to inspect. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Emit the report as one JSON object instead of a human line.
    #[arg(long)]
    pub json: bool,
}

/// Runs `rr refresh` or `rr map` and returns its exit code.
///
/// # Errors
/// Returns I/O, discovery, extraction, and publication failures. Interruption
/// is not among them: it is an exit code, because a user who pressed Ctrl-C
/// does not need to be told that something went wrong.
pub fn run_refresh(args: &RefreshArgs, command: RefreshCommand) -> anyhow::Result<u8> {
    let root = match args.root.clone() {
        Some(root) => root,
        None => std::env::current_dir().context("resolve current directory")?,
    };
    let threads = usize::from(args.threads.unwrap_or(1));
    let mode = if args.full || command == RefreshCommand::Map {
        RefreshMode::Full
    } else {
        RefreshMode::Incremental
    };

    let cancel = install_interrupt();
    let detail = if args.verbose {
        ReportDetail::Verbose
    } else {
        ReportDetail::Summary
    };
    let report = match publish(&root, threads, mode, &cancel) {
        Ok(report) => report,
        Err(Published::Interrupted) => return Ok(exit::INTERRUPTED),
        Err(Published::Conflicts(report)) => {
            if args.json {
                Output::print_text(
                    &render_refresh_json(&report, command, detail)
                        .context("render report as JSON")?,
                )?;
            } else {
                Output::print_error(&report.text.conflict_report())?;
            }
            return Ok(exit::ERROR);
        }
        Err(Published::Failed(error)) => return Err(error),
    };

    if args.json {
        Output::print_text(
            &render_refresh_json(&report, command, detail).context("render report as JSON")?,
        )?;
    } else {
        Output::print_text(&render_refresh_text(&report, command))?;
        if args.verbose {
            Output::print_text(&render_refresh_verbose(&report))?;
        }
    }

    Ok(exit::OK)
}

/// Runs `rr status` and returns its exit code.
///
/// # Errors
/// Returns I/O and discovery failures.
pub fn run_status(args: &StatusArgs) -> anyhow::Result<u8> {
    let root = match args.root.clone() {
        Some(root) => root,
        None => std::env::current_dir().context("resolve current directory")?,
    };

    let cancel = install_interrupt();
    let report = match rr_git::status(&root, &cancel) {
        Ok(report) => report,
        Err(rr_git::Error::Cancelled) => return Ok(exit::INTERRUPTED),
        Err(error) => return Err(anyhow::Error::new(error).context("read repository status")),
    };

    if args.json {
        Output::print_text(&render_status_json(&report).context("render status as JSON")?)?;
    } else {
        Output::print_text(&render_status_text(&report))?;
    }
    Ok(exit::OK)
}

/// Why a run stopped, when the reason is not an ordinary failure.
enum Published {
    /// The user pressed Ctrl-C.
    Interrupted,
    /// Committed files are in a state only a human can resolve.
    ///
    /// Boxed because this arm is the rare one and an enum sized by it would
    /// make every ordinary return path carry a report's worth of stack.
    Conflicts(Box<RunReport>),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Published {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// Publishes the snapshot and the text artifacts derived from it.
///
/// Both happen under one guard, in this order: stage the text and refuse on a
/// conflict before anything is written, publish the snapshot, write the text,
/// then read the whole generation back. A conflict therefore leaves the
/// repository exactly as it was found, snapshot included.
fn publish(
    root: &Path,
    threads: usize,
    mode: RefreshMode,
    cancel: &CancelToken,
) -> Result<RunReport, Published> {
    let budget = DEFAULT_MAP_BUDGET;
    let prepared = match rr_git::prepare(root, threads, mode, cancel) {
        Ok(prepared) => prepared,
        Err(rr_git::Error::Cancelled) => return Err(Published::Interrupted),
        Err(error) => {
            return Err(Published::Failed(
                anyhow::Error::new(error).context("refresh repository"),
            ))
        }
    };

    match prepared {
        rr_git::Refresh::UpToDate {
            report,
            snapshot,
            work_root,
        } => {
            let staged = text_artifacts::stage(&snapshot, &work_root, budget)?;
            if staged.validation().is_up_to_date() {
                return Ok(RunReport {
                    refresh: report,
                    text: TextReport::unchanged(&staged),
                });
            }
            republish_text(root, threads, budget, report)
        }
        rr_git::Refresh::Prepared(mut prepared) => {
            let staged = text_artifacts::stage(prepared.snapshot(), prepared.work_root(), budget)?;
            if !staged.validation().is_publishable() {
                return Err(refusal(prepared.finish(), &staged));
            }
            prepared.publish().map_err(anyhow::Error::new)?;
            let text = text_artifacts::publish(
                &staged,
                prepared.snapshot(),
                prepared.work_root(),
                budget,
            )?;
            Ok(RunReport {
                refresh: prepared.finish(),
                text,
            })
        }
    }
}

/// Brings text artifacts back into agreement with a snapshot that is already
/// published, under the guard and against the snapshot the guard finds.
fn republish_text(
    root: &Path,
    threads: usize,
    budget: u32,
    refresh: RefreshReport,
) -> Result<RunReport, Published> {
    let Some(held) = rr_git::hold(root, threads).map_err(anyhow::Error::new)? else {
        return Ok(RunReport {
            refresh,
            text: TextReport::default(),
        });
    };
    let staged = text_artifacts::stage(held.snapshot(), held.work_root(), budget)?;
    if !staged.validation().is_publishable() {
        return Err(refusal(refresh, &staged));
    }
    let text = text_artifacts::publish(&staged, held.snapshot(), held.work_root(), budget)?;
    Ok(RunReport { refresh, text })
}

/// The refusal, as the same value a successful run reports.
///
/// `finish()` is called rather than skipped: it settles `elapsed_ms`, and a
/// refusal that reported no duration would be the one run whose report a caller
/// cannot compare with any other. Nothing was published, so `snapshot_updated`
/// is false and `RunReport::outcome` reads `refused`.
fn refusal(refresh: RefreshReport, staged: &StagedText) -> Published {
    Published::Conflicts(Box::new(RunReport {
        refresh,
        text: TextReport::refused(staged),
    }))
}

/// Arranges for Ctrl-C to cancel the run rather than kill it.
///
/// A signal that kills the process mid-publish is the one way to leave a
/// half-written snapshot behind, so the handler flips a flag the refresh polls
/// and the run stops at a boundary it chose. Pressing Ctrl-C twice still kills
/// the process, because the second signal arrives while the first is being
/// honoured and the default disposition has been restored.
#[cfg(unix)]
fn install_interrupt() -> CancelToken {
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
    static FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn handle(_signal: libc::c_int) {
        let flag = FLAG.load(Ordering::SeqCst);
        if !flag.is_null() {
            unsafe { &*flag }.store(true, Ordering::SeqCst);
        }
        let next = crate::release_and_die as extern "C" fn(libc::c_int);
        unsafe {
            libc::signal(libc::SIGINT, next as libc::sighandler_t);
        }
    }

    let token = CancelToken::new();
    let raw = std::sync::Arc::into_raw(token.flag()).cast_mut();
    FLAG.store(raw, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, handle as *const () as libc::sighandler_t);
    }
    token
}

/// Elsewhere the run is simply not interruptible, which is honest: a token that
/// nothing ever cancels is better than a handler that pretends to install.
#[cfg(not(unix))]
fn install_interrupt() -> CancelToken {
    CancelToken::new()
}

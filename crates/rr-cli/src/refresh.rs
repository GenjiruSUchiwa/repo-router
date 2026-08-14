//! `rr refresh`, `rr map`, and `rr status` at the command line.
//!
//! All three are one operation seen from three angles: `map` is `refresh
//! --full`, and `status` is a refresh that stops after deciding. They share the
//! report type and the renderers, so the human line and the JSON object cannot
//! disagree about what happened, and neither can `status` disagree with the
//! `refresh` it is describing.

use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use rr_core::cancel::CancelToken;
use rr_core::refresh::{
    render_refresh_json, render_refresh_text, render_status_json, render_status_text,
    RefreshCommand, RefreshMode, RefreshReport,
};

use crate::output::Output;

/// Exit codes shared by the three commands.
///
/// Deliberately only three, and `2` is deliberately not among them. A command
/// that ran and reported a stale index did what it was asked; the answer is in
/// the report, not in whether the process succeeded. Spending an exit code on
/// it would collide with the `2` that `clap` already returns for a mistyped
/// flag, leaving a script unable to tell "your index is out of date" from "you
/// misspelled `--threads`".
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
    // Rejected by the parser rather than here, so a thread count of zero is a
    // usage error with `clap`'s own exit code instead of a failure that looks
    // like the repository was at fault.
    let threads = usize::from(args.threads.unwrap_or(1));

    // `rr map` means "rebuild everything" by name, so it does not offer the
    // flag: a `--full` that could only ever be true is a flag with no meaning,
    // and a `--full=false` that quietly did nothing would be worse.
    let mode = if args.full || command == RefreshCommand::Map {
        RefreshMode::Full
    } else {
        RefreshMode::Incremental
    };

    let cancel = install_interrupt();
    let report = match rr_git::refresh(&root, threads, mode, &cancel) {
        Ok(report) => report,
        Err(rr_git::Error::Cancelled) => return Ok(exit::INTERRUPTED),
        Err(error) => return Err(anyhow::Error::new(error).context("refresh repository")),
    };

    if args.json {
        Output::print_text(
            &render_refresh_json(&report, command).context("render report as JSON")?,
        )?;
    } else {
        Output::print_text(&render_refresh_text(&report, command))?;
        if args.verbose {
            Output::print_text(&verbose_lines(&report))?;
        }
    }

    // A conflicted path is reported through `conflicted`, not through the exit
    // code. The refresh succeeded and published, which is what the code says.
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

    // Producing the report *is* the job, so producing one is success whatever
    // it says. Callers branch on `snapshot`, which distinguishes five states
    // where an exit code could only ever distinguish two.
    Ok(exit::OK)
}

/// The counter breakdown behind the summary line.
fn verbose_lines(report: &RefreshReport) -> String {
    format!(
        "  plan: {} mode, {} changed, {} removed, {} renamed, {} conflicted\n  \
         work: {} reparsed, {} cached, {} content reads, {} degraded\n  \
         cache: {} corrupt\n  snapshot: {}",
        report.mode.as_str(),
        report.changed,
        report.removed,
        report.renamed,
        report.conflicted,
        report.reparsed,
        report.cached,
        report.content_reads,
        report.degraded,
        report.cache_corrupt,
        if report.snapshot_updated {
            "republished"
        } else {
            "unchanged on disk"
        }
    )
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

    // The handler cannot own an `Arc`, so it reaches the flag through a raw
    // pointer to one that is deliberately never dropped. A leak of a single
    // bool that lives exactly as long as the process is the cheapest way to
    // guarantee the pointer stays valid for as long as a signal can arrive.
    static FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn handle(_signal: libc::c_int) {
        let flag = FLAG.load(Ordering::SeqCst);
        if !flag.is_null() {
            // Storing to an `AtomicBool` is async-signal-safe; nothing else
            // here allocates, locks, or formats.
            unsafe { &*flag }.store(true, Ordering::SeqCst);
        }
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
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

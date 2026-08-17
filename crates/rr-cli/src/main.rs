mod check;
mod init;
mod output;
mod query;
mod refresh;
mod text_artifacts;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use output::Output;
use refresh::{exit, RefreshArgs, StatusArgs};
use rr_core::refresh::RefreshCommand;

const VERSION_STR: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("RR_GIT_HASH"), ")");

#[derive(Parser, Debug)]
#[command(
    name = "rr",
    version = VERSION_STR,
    about = "Repository router for AI coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Print the version, or the language table with `--languages`.
    Version(VersionArgs),
    /// Bring the snapshot back into agreement with the repository.
    Refresh(RefreshArgs),
    /// Rebuild the whole snapshot. `rr refresh --full` under an older name.
    Map(RefreshArgs),
    /// Report how the repository and its snapshot relate, changing neither.
    Status(StatusArgs),
    /// Install the agent navigation contract. Safe to run again.
    Init(init::Args),
    /// Report every numbered diagnostic this repository's artifacts raise.
    Check(check::Args),
    Query(query::Args),
}

#[derive(clap::Args, Debug)]
struct VersionArgs {
    /// List every language rr recognises and the tier it reaches.
    #[arg(long)]
    languages: bool,
}

fn main() -> ExitCode {
    install_signal_cleanup();

    let cli = Cli::parse();
    match cli.command {
        Commands::Version(args) => {
            let printed = if args.languages {
                Output::print_languages()
            } else {
                Output::print_version(env!("CARGO_PKG_VERSION"), env!("RR_GIT_HASH"))
            };
            if let Err(err) = printed {
                diagnose(&format!("rr: {err}"));
                return ExitCode::from(1);
            }
            ExitCode::from(0)
        }
        Commands::Refresh(args) => finish(
            "refresh",
            refresh::run_refresh(&args, RefreshCommand::Refresh),
        ),
        Commands::Map(args) => finish("map", refresh::run_refresh(&args, RefreshCommand::Map)),
        Commands::Status(args) => finish("status", refresh::run_status(&args)),
        Commands::Init(args) => finish("init", init::run(&args)),
        Commands::Check(args) => match check::run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                diagnose(&format!("rr: check: {}", one_line(&err)));
                ExitCode::from(check::NOT_EVALUATED)
            }
        },
        Commands::Query(args) => match query::run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                diagnose(&format!("rr: query: {}", one_line(&err)));
                ExitCode::from(1)
            }
        },
    }
}

/// Points every signal that ends a run at a handler that dies the way
/// `SIG_DFL` would, having first let go of what the death would strand.
///
/// Termination runs no destructor, so a run killed while it holds the
/// publication claim leaves the lock file behind — and acquisition fails on
/// that file's mere existence, so every later refresh of that repository is
/// refused until a human deletes it. Ctrl-C is the common way to arrive there;
/// a `kill`, a closed terminal and a broken pipe are the others.
///
/// SIGPIPE is also here for its own reason. Rust ignores it, so clap's prints
/// panic on a closed pipe (exit 101), and taking a disposition covers those
/// writes too. The rest already terminate on their own and are listed only for
/// the cleanup.
#[cfg(unix)]
fn install_signal_cleanup() {
    let handler = release_and_die as extern "C" fn(libc::c_int);
    for signal in [
        libc::SIGPIPE,
        libc::SIGINT,
        libc::SIGTERM,
        libc::SIGQUIT,
        libc::SIGHUP,
    ] {
        unsafe {
            libc::signal(signal, handler as libc::sighandler_t);
        }
    }
}

/// Releases what `Drop` will not be reached to release, then re-raises the
/// signal against the default disposition.
///
/// Re-raising rather than exiting is the whole point: a caller reads 141 as
/// "SIGPIPE killed it" and 130 as "somebody pressed Ctrl-C", and a handler that
/// returned an exit code instead would answer a question nobody asked. The
/// signal stays blocked until this returns, so the second delivery lands on
/// `SIG_DFL` and terminates the process.
#[cfg(unix)]
pub(crate) extern "C" fn release_and_die(signal: libc::c_int) {
    rr_git::release_locks_signal_safe();
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// No-op: this platform has none of these signals.
#[cfg(not(unix))]
fn install_signal_cleanup() {}

/// Writes a diagnostic to stderr. Ignores write errors: if stderr is gone,
/// there is nowhere else to say so.
fn diagnose(message: &str) {
    let _ = Output::print_error(message);
}

/// Turns one command's result into a process exit code.
///
/// Errors all leave by this door so that no command can invent its own
/// diagnostic shape, and so a failure can never be mistaken for the `2` that
/// means "it worked and the answer is not one to act on".
fn finish(command: &str, result: anyhow::Result<u8>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            diagnose(&format!("rr: {command}: {}", one_line(&err)));
            ExitCode::from(exit::ERROR)
        }
    }
}

/// Flattens an error chain into a single diagnostic line.
///
/// A failure deep in the stack can carry text we did not author — an external
/// filter's stderr, an OS message in another locale — and a newline in it would
/// turn one diagnostic into two, which is exactly the shape a caller reading
/// stderr line by line would misread as a second failure.
fn one_line(err: &anyhow::Error) -> String {
    let rendered = format!("{err:#}");
    let mut line = String::with_capacity(rendered.len());
    let mut pending_space = false;
    for character in rendered.chars() {
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            pending_space = !line.is_empty();
            continue;
        }
        if pending_space {
            if character.is_whitespace() {
                continue;
            }
            line.push(' ');
            pending_space = false;
        }
        line.push(character);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn a_multi_line_failure_still_reports_as_one_diagnostic() {
        let inner = anyhow::anyhow!("filter said:\n  bad magic\r\n  at offset 3\u{2028}stop");
        let err = inner.context("acquire source for src/lib.rs");

        let rendered = one_line(&err);

        assert_eq!(rendered.lines().count(), 1);
        assert_eq!(
            rendered,
            "acquire source for src/lib.rs: filter said: bad magic at offset 3 stop"
        );
    }

    #[test]
    fn an_ordinary_failure_survives_unchanged() {
        let err = anyhow::anyhow!("no such file: src/lib.rs");

        assert_eq!(one_line(&err), "no such file: src/lib.rs");
    }
}

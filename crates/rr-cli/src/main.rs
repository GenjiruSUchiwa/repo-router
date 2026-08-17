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
    Version,
    /// Bring the snapshot back into agreement with the repository.
    Refresh(RefreshArgs),
    /// Rebuild the whole snapshot. `rr refresh --full` under an older name.
    Map(RefreshArgs),
    /// Report how the repository and its snapshot relate, changing neither.
    Status(StatusArgs),
    /// Install the agent navigation contract. Safe to run again.
    Init(init::Args),
    Query(query::Args),
}

fn main() -> ExitCode {
    restore_default_sigpipe();

    let cli = Cli::parse();
    match cli.command {
        Commands::Version => {
            if let Err(err) = Output::print_version(env!("CARGO_PKG_VERSION"), env!("RR_GIT_HASH"))
            {
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
        Commands::Query(args) => match query::run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                diagnose(&format!("rr: query: {}", one_line(&err)));
                // Exit 1 for all errors: SPEC reserves 2 for the candidates
                // outcome, so errors must not collide with it.
                ExitCode::from(1)
            }
        },
    }
}

/// Puts SIGPIPE back to its default disposition, before anything can print.
///
/// Rust's runtime ignores SIGPIPE, which turns "the consumer stopped reading"
/// into an `EPIPE` that every writer has to notice. The `println!` family does
/// not notice: it panics — `failed printing to stdout: Broken pipe (os error
/// 32)`, exit 101 — and that panic is the failure `docs/OBSERVATIONS.md` §9.6
/// recorded in Radar and issue #1 asked this binary not to repeat.
///
/// Noticing `ErrorKind::BrokenPipe` at rr's own write boundary would not be
/// enough, because rr is not the only writer in this process. `rr --help`,
/// `rr --version` and every usage error are printed by `clap`, and no boundary
/// this crate owns can reach those writes. A disposition covers every write in
/// the process, including the ones in dependencies, and covers them from the
/// first statement of `main` rather than from wherever the first `Output` call
/// happens to be.
///
/// The price is that the process is *terminated*, not unwound: no destructor
/// runs, so nothing may be printed while the publication guard is held. See the
/// `exit` module in `refresh.rs`, and `tests/broken_pipe.rs`, which pins it.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // Not error-checked, and not checkable: `signal` reports `SIG_ERR` only for
    // `SIGKILL` and `SIGSTOP`, so there is no failure to report and no half
    // change to undo. `signal` rather than `sigaction` because the one trap in
    // `signal` is the BSD/SysV disagreement over re-arming a handler, and
    // `SIG_DFL` installs no handler.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Elsewhere there is no SIGPIPE to restore, and nothing here pretends there is.
///
/// A closed pipe surfaces as `ErrorKind::BrokenPipe` from the write itself, and
/// the `io::Result` every `Output` method returns already carries it to `main`,
/// which reports it as the ordinary `1`. That path is stated rather than
/// implemented because no CI job builds it — the matrix is `macos-14`,
/// `ubuntu-latest` and `ubuntu-24.04-arm` — and a broken-pipe path nothing
/// compiles is a claim nothing checks. Issue #46 holds the ledger entry.
#[cfg(not(unix))]
fn restore_default_sigpipe() {}

/// Reports a failure on stderr, or gives up reporting it.
///
/// The write result is deliberately discarded. A diagnostic has exactly one
/// channel, and if that channel is gone there is nowhere left to say so:
/// writing the complaint to stdout instead would drop it into the middle of the
/// report a script is parsing, which is the interleaving `Output::print_error`
/// exists to prevent. The exit code is the report of last resort.
///
/// On unix this line is not even reached for the case that motivates it: a
/// write to a pipe whose reader has gone raises SIGPIPE, and `main` restored
/// that disposition before anything could print. The `Result` matters on the
/// platforms `restore_default_sigpipe` deliberately does nothing on, where an
/// `eprintln!` here would panic instead.
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
        // U+2028/U+2029 are not control characters but still break a line for
        // any reader that splits on Unicode line boundaries rather than on LF.
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            pending_space = !line.is_empty();
            continue;
        }
        if pending_space {
            // The whitespace that followed a break was indentation for a line
            // that no longer exists, so it joins the break rather than the text.
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

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

/// Restores SIGPIPE to `SIG_DFL`. Rust ignores it, so clap's prints panic
/// on a closed pipe (exit 101). A disposition covers those writes too.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// No-op: this platform has no SIGPIPE.
#[cfg(not(unix))]
fn restore_default_sigpipe() {}

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

//! `rr` and a consumer that stopped reading.
//!
//! An agent runs `rr` through a pipe, so a pipe whose reader has gone is the
//! ordinary case and not the exotic one. Every test here writes into a pipe
//! whose read end was closed *before* the child was spawned, so none of them
//! depends on beating a consumer that is on its way out — the race that makes a
//! broken-pipe test pass on a laptop and flake in CI.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::os::unix::io::FromRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

mod common;

/// Which of the child's streams nobody is reading.
#[derive(Clone, Copy, Debug)]
enum Closed {
    Stdout,
    Stderr,
}

/// A stream the child can only fail to write to.
///
/// The read end is closed before the child exists, so its very first byte is a
/// write to a pipe with no reader. Dropping a `Stdio::piped()` handle after
/// spawning produces the same condition only when the parent wins the race.
fn closed_pipe() -> Stdio {
    let mut fds = [0 as libc::c_int; 2];
    // `pipe` fills exactly the two descriptors of the array it is handed.
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(
        created,
        0,
        "pipe(2) failed: {}",
        std::io::Error::last_os_error()
    );
    // Nothing else owns the read end, and nothing will ever read it.
    unsafe { libc::close(fds[0]) };
    // Ownership of the write end moves into the child's stdio.
    unsafe { Stdio::from_raw_fd(fds[1]) }
}

/// Runs `rr` with one stream connected to a pipe nobody reads and the other
/// captured, so a regression to a panic has somewhere to be seen.
///
/// `RUST_BACKTRACE=1` is deliberate: if this ever regresses, the assertion on
/// an empty stream fails with the whole backtrace in the message.
fn run_into_closed(dir: &Path, args: &[&str], closed: Closed) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rr"));
    command
        .args(args)
        .current_dir(dir)
        .env("RUST_BACKTRACE", "1");
    match closed {
        Closed::Stdout => command.stdout(closed_pipe()).stderr(Stdio::piped()),
        Closed::Stderr => command.stderr(closed_pipe()).stdout(Stdio::piped()),
    };
    command.spawn().unwrap().wait_with_output().unwrap()
}

/// A repository with one indexed symbol to answer for.
fn mapped_repo() -> TempDir {
    let repo = common::empty_repo();
    common::write(
        repo.path(),
        "src/auth/token.rs",
        "pub fn verify_token() -> bool { true }\n",
    );
    common::commit_all(repo.path(), "init");
    assert!(common::run(repo.path(), &["map"]).status.success());
    repo
}

#[test]
fn every_command_dies_quietly_when_the_consumer_closed_stdout() {
    let repo = mapped_repo();

    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["version"],
        vec!["status"],
        vec!["status", "--json"],
        vec!["refresh"],
        vec!["refresh", "--verbose"],
        vec!["map"],
        vec!["query", "verify_token"],
        vec!["query", "--json", "verify_token"],
        vec!["query", "--source", "verify_token"],
        vec!["query", "--explain", "--source", "verify_token"],
    ] {
        let output = run_into_closed(repo.path(), &args, Closed::Stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.signal(),
            Some(libc::SIGPIPE),
            "rr {args:?} did not die of SIGPIPE (status {:?}, stderr {stderr:?})",
            output.status
        );
        assert_eq!(
            output.status.code(),
            None,
            "rr {args:?} returned an exit code, so it was never signalled"
        );
        assert!(stderr.is_empty(), "rr {args:?} said {stderr:?} on stderr");
    }
}

#[test]
fn a_diagnostic_written_to_a_closed_stderr_is_silent_too() {
    let repo = mapped_repo();

    for args in [
        // crates/rr-cli/src/main.rs:63 — the query arm's own diagnostic.
        vec!["query", ""],
        // clap's unrecognised-subcommand error, before any command runs.
        vec!["nosuchcommand"],
        // clap's value error, which the parser raises rather than `run_refresh`.
        vec!["refresh", "--threads", "0"],
        // crates/rr-cli/src/main.rs:81 — `finish`, the door every command error leaves by.
        vec!["refresh", "--root", "/definitely/not/a/repository"],
    ] {
        let output = run_into_closed(repo.path(), &args, Closed::Stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            output.status.signal(),
            Some(libc::SIGPIPE),
            "rr {args:?} did not die of SIGPIPE (status {:?}, stdout {stdout:?})",
            output.status
        );
        assert!(
            stdout.is_empty(),
            "rr {args:?} wrote {stdout:?} to stdout while failing"
        );
    }
}

#[test]
fn a_closed_stderr_costs_a_silent_command_nothing() {
    let repo = mapped_repo();

    let output = run_into_closed(repo.path(), &["query", "verify_token"], Closed::Stderr);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a successful query was signalled by a stderr nobody was reading"
    );
}

#[test]
fn a_broken_pipe_prints_no_panic_and_no_backtrace() {
    let repo = mapped_repo();

    let output = run_into_closed(
        repo.path(),
        &["query", "--source", "verify_token"],
        Closed::Stdout,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("panicked"),
        "a broken pipe is not an error to report: {stderr}"
    );
    assert_ne!(
        output.status.code(),
        Some(101),
        "101 is how a panicking Rust process exits"
    );
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

#[test]
fn a_signalled_run_leaves_no_publication_lock_behind() {
    let repo = mapped_repo();
    // Something to actually rebuild, so this run takes the guard rather than
    // reporting the snapshot already current.
    common::write(repo.path(), "src/auth/session.rs", "pub fn session() {}\n");
    common::commit_all(repo.path(), "add session");

    let killed = run_into_closed(repo.path(), &["map"], Closed::Stdout);
    assert_eq!(killed.status.signal(), Some(libc::SIGPIPE));

    // Named by `workspace::publication_lock_path` as `.rr/local/publication`;
    // matched by prefix so the lock crate's own suffix is not restated here.
    let leftovers: Vec<String> = std::fs::read_dir(repo.path().join(".rr").join("local"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("publication"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the publication guard leaked {leftovers:?}: something printed while it was held"
    );

    let again = common::run(repo.path(), &["map"]);
    assert!(
        again.status.success(),
        "a later refresh was refused, so the claim was never released: {}",
        common::stderr(&again)
    );
}

#[test]
fn a_signalled_run_still_published_what_it_had_already_written() {
    let repo = mapped_repo();
    common::write(repo.path(), "src/auth/session.rs", "pub fn session() {}\n");
    common::commit_all(repo.path(), "add session");

    let killed = run_into_closed(repo.path(), &["map"], Closed::Stdout);
    assert_eq!(killed.status.signal(), Some(libc::SIGPIPE));

    let answer = common::run(repo.path(), &["query", "--json", "session"]);
    assert_eq!(common::code(&answer), 0);
    assert_eq!(
        common::json(&answer)["anchor"]["path"],
        "src/auth/session.rs",
        "the run was killed writing its report, not writing its snapshot"
    );
}

#[test]
fn a_consumer_that_reads_everything_is_unaffected() {
    let repo = mapped_repo();

    let output = common::run(repo.path(), &["query", "--source", "verify_token"]);

    assert_eq!(common::code(&output), 0);
    assert!(common::stderr(&output).is_empty());
    assert!(common::stdout(&output).starts_with("FINAL SOURCE ANCHOR (copy exactly): "));
}

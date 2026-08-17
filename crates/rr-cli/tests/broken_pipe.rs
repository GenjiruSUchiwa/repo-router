//! Broken-pipe contract: a consumer that stopped reading.
//!
//! Every test writes into a pipe whose read end was closed before spawn,
//! so none of them race a consumer on its way out.

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

/// Write end of a pipe whose read end is already closed.
fn closed_pipe() -> Stdio {
    let mut fds = [0 as libc::c_int; 2];
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(
        created,
        0,
        "pipe(2) failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { libc::close(fds[0]) };
    unsafe { Stdio::from_raw_fd(fds[1]) }
}

/// Run `rr` with one stream on a closed pipe and the other captured.
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
        vec!["query", ""],
        vec!["nosuchcommand"],
        vec!["refresh", "--threads", "0"],
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
    common::write(repo.path(), "src/auth/session.rs", "pub fn session() {}\n");
    common::commit_all(repo.path(), "add session");

    let killed = run_into_closed(repo.path(), &["map"], Closed::Stdout);
    assert_eq!(killed.status.signal(), Some(libc::SIGPIPE));

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

/// The claim also has to survive its holder being killed *while it is held*,
/// which the test above cannot arrange: nothing prints between the claim and
/// the report, so no closed pipe reaches `rr` inside that window. A Git clean
/// filter does. `head -c 8` stops reading long before `rr` has finished
/// streaming the blob to it, and the EPIPE on that write raises SIGPIPE
/// against the same process-wide disposition — from under the guard.
///
/// That `rr` dies here at all is a separate defect, filed as #59: a filter that
/// exits early is survivable, and `rr-git` is written to report it as
/// `Error::Content`. This test pins only what becomes of the lock. When #59 is
/// fixed the signal assertion fails loudly rather than passing on a window it
/// no longer enters, and the test needs a new vector.
#[test]
fn a_signal_raised_while_the_claim_is_held_still_releases_it() {
    let repo = common::empty_repo();
    common::git(repo.path(), &["config", "filter.trunc.clean", "head -c 8"]);
    common::write(repo.path(), ".gitattributes", "*.rs filter=trunc\n");
    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(1));
    common::commit_all(repo.path(), "add token behind a truncating filter");
    // Dirty the worktree copy: a clean file never reaches the filter at all.
    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(2));

    let killed = common::run(repo.path(), &["map"]);
    assert_eq!(
        killed.status.signal(),
        Some(libc::SIGPIPE),
        "the clean filter no longer kills rr, so this no longer covers the window it exists for"
    );

    let lock = repo
        .path()
        .join(".rr")
        .join("local")
        .join("publication.lock");
    assert!(!lock.exists(), "the claim outlived the run that held it");

    // With the filter gone, only a leaked claim could refuse the next run.
    common::git(repo.path(), &["config", "--unset", "filter.trunc.clean"]);
    std::fs::remove_file(repo.path().join(".gitattributes")).unwrap();
    let again = common::run(repo.path(), &["map"]);
    assert!(
        again.status.success(),
        "a later refresh was refused, so the claim was never released: {}",
        common::stderr(&again)
    );
}

/// A source file with more in it than the filter will read before exiting, so
/// the write that feeds it is still going when the read end closes.
fn wider_than_a_pipe(seed: usize) -> String {
    format!(
        "pub fn verify_token() -> bool {{ true }}\n// {}\n",
        "x".repeat(256 * 1024 + seed)
    )
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

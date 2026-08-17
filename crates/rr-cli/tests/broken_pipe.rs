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

/// A Git clean filter that stops reading used to raise SIGPIPE against the
/// process-wide disposition, from under the publication claim. The run died
/// with 141 — the code that means "the consumer went away" — and left no
/// diagnostic. `gix-filter` treats that early close as success when the
/// driver exits 0; once the write is allowed to fail, so does `rr map`.
///
/// The lock-under-signal coverage this used to piggy-back on lives in
/// `interrupted.rs`. What remains here is the #59 contract: the filter is
/// not a consumer, so it must not produce 141.
#[test]
fn a_clean_filter_that_stops_reading_does_not_kill_the_run() {
    let repo = common::empty_repo();
    common::git(repo.path(), &["config", "filter.trunc.clean", "head -c 8"]);
    common::write(repo.path(), ".gitattributes", "*.rs filter=trunc\n");
    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(1));
    common::commit_all(repo.path(), "add token behind a truncating filter");

    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(2));

    let mapped = common::run(repo.path(), &["map"]);
    assert_eq!(
        mapped.status.signal(),
        None,
        "a filter that closed stdin killed rr (status {:?}, stderr {:?})",
        mapped.status,
        common::stderr(&mapped)
    );
    assert_eq!(
        common::code(&mapped),
        0,
        "rr map failed instead of taking the filter's output: {}",
        common::stderr(&mapped)
    );

    let lock = repo
        .path()
        .join(".rr")
        .join("local")
        .join("publication.lock");
    assert!(!lock.exists(), "the claim outlived the run that held it");
}

/// The filter runs on a second path, and it is the one a real repository takes.
/// A truncating filter makes every tracked file's recorded size disagree with
/// the file on disk, so `gix-status` cannot settle the entry by `stat` and
/// streams the worktree copy through the filter itself to compare hashes. That
/// write is outside `convert_to_git`, so guarding only that one left the
/// commonest case — a clean worktree, nothing modified — still dying with 141.
#[test]
fn a_clean_filter_does_not_kill_a_run_over_an_unmodified_worktree() {
    let repo = common::empty_repo();
    common::git(repo.path(), &["config", "filter.trunc.clean", "head -c 8"]);
    common::write(repo.path(), ".gitattributes", "*.rs filter=trunc\n");
    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(1));
    common::commit_all(repo.path(), "add token behind a truncating filter");

    common::write(repo.path(), "src/token.rs", &wider_than_a_pipe(1));

    let mapped = common::run(repo.path(), &["map"]);
    assert_eq!(
        mapped.status.signal(),
        None,
        "the status scan's own filter write killed rr (status {:?}, stderr {:?})",
        mapped.status,
        common::stderr(&mapped)
    );
    assert_eq!(common::code(&mapped), 0, "{}", common::stderr(&mapped));

    let observed = common::run(repo.path(), &["status"]);
    assert_eq!(
        observed.status.signal(),
        None,
        "rr status died on the same write (status {:?})",
        observed.status
    );
    assert_eq!(common::code(&observed), 0, "{}", common::stderr(&observed));
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

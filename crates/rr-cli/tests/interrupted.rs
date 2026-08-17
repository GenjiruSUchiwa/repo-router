//! A run cut short while it is publishing must not strand its claim.
//!
//! Termination runs no destructor, so the lock file the guard would have
//! removed survives — and acquisition fails on its mere existence, which leaves
//! the repository refusing every later refresh until a human deletes the file.
//!
//! Ctrl-C is the exception and gets its own test: `refresh` catches the first
//! SIGINT, so that run stops at a boundary and unwinds normally. Everything
//! else here dies where it stands and relies on the handler `main` installs.
//!
//! Each test waits until the claim is genuinely on disk before signalling, so
//! none of them guess at the window they need to land in.
//!
//! A *second* Ctrl-C is not covered. It is handled — the cooperative handler
//! hands SIGINT back to the one that cleans up rather than to `SIG_DFL` — but
//! the window between that re-arming and the end of the shutdown is a couple of
//! milliseconds wide, and signals do not queue, so a test aiming at it lands
//! four times out of six. A test that flaky is worse than this paragraph.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

mod common;

/// Enough source files that building a snapshot outlasts the poll below. The
/// claim is taken before the build and released after it publishes, so this is
/// also how wide the window to signal into is.
const FILES: usize = 400;

/// How long to wait for the claim to appear before giving up on this attempt.
const PATIENCE: Duration = Duration::from_mins(1);

/// A pause between seeing the claim and signalling, to skip a window that is
/// not ours to close.
///
/// `gix-tempfile` creates the lock file and only then enters it in the registry
/// the cleanup sweeps (`handle.rs`, `at_path`: `tempfile_in` precedes
/// `REGISTRY.insert`). A signal in between finds a file on disk that nothing
/// knows about, and it survives. That gap is microseconds wide inside a build
/// that runs for seconds, and it lives upstream — signalling into it here would
/// test `gix-tempfile`'s registration, not our handler. What these tests are
/// for is the rest of the run, which is all of it.
const PAST_REGISTRATION: Duration = Duration::from_millis(100);

/// A repository with enough in it to take a measurable time to map.
fn wide_repo() -> TempDir {
    let repo = common::empty_repo();
    for index in 0..FILES {
        common::write(
            repo.path(),
            &format!("src/mod_{index}.rs"),
            &format!(
                "pub fn handler_{index}() -> bool {{ true }}\n\
                 pub struct Thing{index} {{ pub id: u64 }}\n"
            ),
        );
    }
    common::commit_all(repo.path(), "a repository worth mapping");
    repo
}

fn claim_of(root: &Path) -> PathBuf {
    root.join(".rr").join("local").join("publication.lock")
}

fn map_in_background(root: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_rr"))
        .arg("map")
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn send(child: &Child, signal: libc::c_int) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "kill(2) failed");
}

/// Blocks until `child` holds the claim. False if it finished without our
/// seeing it, which makes the attempt worthless rather than failed.
fn wait_until_publishing(child: &mut Child, root: &Path) -> bool {
    let claim = claim_of(root);
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if claim.exists() {
            std::thread::sleep(PAST_REGISTRATION);
            return child.try_wait().unwrap().is_none();
        }
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    false
}

/// Runs `rr map` and signals it once the claim is observably held, retrying
/// rather than racing: an attempt where the run published first proves nothing.
fn signal_a_run_holding_the_claim(root: &Path, signal: libc::c_int) -> ExitStatus {
    for _ in 0..5 {
        let mut child = map_in_background(root);
        if !wait_until_publishing(&mut child, root) {
            child.wait().unwrap();
            std::fs::remove_dir_all(root.join(".rr")).unwrap();
            continue;
        }

        send(&child, signal);
        return child.wait().unwrap();
    }
    panic!("never caught a run while it held the claim; is {FILES} files still enough?");
}

/// Asserts the repository is usable again, with nothing left to clean by hand.
fn assert_the_claim_was_released(root: &Path) {
    assert!(
        !claim_of(root).exists(),
        "the claim outlived the run that held it, so every later refresh is refused"
    );
    let again = common::run(root, &["map"]);
    assert!(
        again.status.success(),
        "a later refresh was refused: {}",
        common::stderr(&again)
    );
}

/// The cooperative path, which predates the handler: the run is asked to stop,
/// stops at a boundary, and unwinds — so `Drop` releases the claim itself.
#[test]
fn ctrl_c_while_publishing_stops_the_run_and_releases_the_claim() {
    let repo = wide_repo();

    let status = signal_a_run_holding_the_claim(repo.path(), libc::SIGINT);

    assert_eq!(
        status.code(),
        Some(130),
        "an interrupted refresh returns 128 + SIGINT rather than being killed"
    );
    assert_eq!(
        status.signal(),
        None,
        "the first Ctrl-C is caught, not fatal"
    );
    assert_the_claim_was_released(repo.path());
}

#[test]
fn a_kill_while_publishing_releases_the_claim() {
    let repo = wide_repo();

    let status = signal_a_run_holding_the_claim(repo.path(), libc::SIGTERM);

    assert_eq!(status.signal(), Some(libc::SIGTERM));
    assert_the_claim_was_released(repo.path());
}

/// Closing the terminal on a run is the same problem arriving by another door.
#[test]
fn a_closed_terminal_while_publishing_releases_the_claim() {
    let repo = wide_repo();

    let status = signal_a_run_holding_the_claim(repo.path(), libc::SIGHUP);

    assert_eq!(status.signal(), Some(libc::SIGHUP));
    assert_the_claim_was_released(repo.path());
}

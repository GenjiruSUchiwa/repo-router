//! Confined, symlink-refusing opening of one worktree entry.
//!
//! Source verification uses a stricter policy than [`crate::oid_of`]: a symlink
//! is never source, even though Git stores its link text as blob content. The
//! entry is reached by walking down from an opened root so a swapped component
//! cannot redirect the read, and every byte is then read through the returned
//! handle rather than through the path a second time.
//!
//! The handle's `stat` is only reported for the two decisions taken before
//! reading — is this a regular file, and is it already over the verification
//! cap. Detecting a change *during* the read needs no metadata: content that
//! hashes to the indexed identity is the indexed content, whether or not it was
//! read in one piece.
//!
//! Platform primitives stay private to this module. Unix walks with
//! descriptor-relative no-follow opens. Every target this project builds and
//! tests is Unix, so anywhere else fails closed rather than serving content
//! under a guarantee that was never implemented for it.

use std::fs::File;
use std::path::Path;

use rr_core::path::RelPath;
use rr_core::verify::ContentPathState;

use crate::Result;

#[cfg(unix)]
#[path = "safe_open/unix.rs"]
mod imp;

#[cfg(not(unix))]
#[path = "safe_open/unsupported.rs"]
mod imp;

pub(crate) use imp::EntryStat;

/// A regular file opened under the no-symlink policy, with its `stat`.
pub(crate) struct OpenedFile {
    pub(crate) file: File,
    pub(crate) stat: EntryStat,
}

/// Either an opened regular file or the reason it cannot be one.
pub(crate) enum OpenOutcome {
    Opened(OpenedFile),
    Refused(ContentPathState),
}

/// Opens `rel` beneath `root`, refusing anything that is not a regular file
/// reached without following a single symlink or reparse point.
///
/// # Errors
/// Returns [`crate::Error::Io`] for permission failures and other I/O errors,
/// which are execution errors rather than expected refusals.
pub(crate) fn open_regular_file(root: &Path, rel: &RelPath) -> Result<OpenOutcome> {
    imp::open_regular_file(root, rel)
}

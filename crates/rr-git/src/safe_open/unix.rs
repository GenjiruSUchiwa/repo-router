//! Descriptor-relative, no-follow opening on Unix.
//!
//! Every component is opened relative to the previously opened directory
//! descriptor with `O_NOFOLLOW`, so the walk cannot be redirected by a symlink
//! swapped in mid-traversal — which a `symlink_metadata` pre-check followed by
//! an ordinary open would not survive. `O_NONBLOCK` keeps a FIFO or device from
//! blocking the open it is about to be refused for.

use std::fs::File;
use std::path::Path;

use rr_core::path::RelPath;
use rr_core::verify::ContentPathState;
use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::{OpenOutcome, OpenedFile};
use crate::{Error, Result};

/// What `fstat` on the opened handle says about the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryStat {
    regular: bool,
    size: u64,
}

impl EntryStat {
    pub(crate) const fn is_regular_file(self) -> bool {
        self.regular
    }

    pub(crate) const fn size(self) -> u64 {
        self.size
    }
}

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

pub(crate) fn open_regular_file(root: &Path, rel: &RelPath) -> Result<OpenOutcome> {
    let root_flags = DIRECTORY_FLAGS.difference(OFlags::NOFOLLOW);
    let mut directory = rustix::fs::open(root, root_flags, Mode::empty()).map_err(io_error)?;

    let (parents, name) = split_parents(rel.as_str());
    for component in parents {
        directory = match rustix::fs::openat(&directory, component, DIRECTORY_FLAGS, Mode::empty())
        {
            Ok(opened) => opened,
            Err(errno) => return refuse(&directory, component, errno),
        };
    }

    let opened: OwnedFd = match rustix::fs::openat(&directory, name, FILE_FLAGS, Mode::empty()) {
        Ok(opened) => opened,
        Err(errno) => return refuse(&directory, name, errno),
    };

    let file = File::from(opened);
    let stat = stat_of(&file)?;
    if stat.is_regular_file() {
        Ok(OpenOutcome::Opened(OpenedFile { file, stat }))
    } else {
        Ok(OpenOutcome::Refused(ContentPathState::NotRegular))
    }
}

/// Inspects the opened handle, never the path: a replacement after the open
/// cannot make a non-regular entry look regular here.
fn stat_of(file: &File) -> Result<EntryStat> {
    let stat = rustix::fs::fstat(file).map_err(io_error)?;
    Ok(EntryStat {
        regular: FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
        size: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
    })
}

/// Splits a relative path into its directory components and its final name.
///
/// Empty components are dropped so a flat path walks no directories at all;
/// `"a".split('/')` yields one empty component, which would be opened as `""`
/// and reported as a missing file.
fn split_parents(path: &str) -> (impl Iterator<Item = &str>, &str) {
    let (parents, name) = path.rsplit_once('/').unwrap_or(("", path));
    (parents.split('/').filter(|part| !part.is_empty()), name)
}

/// Maps the errors that are expected states rather than execution failures.
fn refuse(directory: &OwnedFd, component: &str, errno: Errno) -> Result<OpenOutcome> {
    let state = match errno {
        Errno::NOENT => ContentPathState::Missing,
        // A symlink met with `O_NOFOLLOW`: `ELOOP` on Linux, `EMLINK` on some
        // BSDs, and `ENOTDIR` on macOS when a directory was also required.
        Errno::LOOP | Errno::MLINK => ContentPathState::Symlink,
        // A socket has no reader to open.
        Errno::NXIO => ContentPathState::NotRegular,
        // Ambiguous: the component may be a symlink refused by `O_NOFOLLOW`, or
        // simply the wrong kind of entry. Only naming it needs a second look,
        // and the entry is refused either way.
        Errno::NOTDIR | Errno::ISDIR => kind_of(directory, component, errno)?,
        other => return Err(io_error(other)),
    };
    Ok(OpenOutcome::Refused(state))
}

/// Names an already-refused component, without following it.
fn kind_of(directory: &OwnedFd, component: &str, errno: Errno) -> Result<ContentPathState> {
    match rustix::fs::statat(directory, component, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink => {
            Ok(ContentPathState::Symlink)
        }
        Ok(_) => Ok(ContentPathState::NotRegular),
        Err(Errno::NOENT) => Ok(ContentPathState::Missing),
        // The component could not be named; report why the open failed.
        Err(_) => Err(io_error(errno)),
    }
}

fn io_error(errno: Errno) -> Error {
    Error::Io(std::io::Error::from(errno))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn splits_nested_and_flat_paths() {
        let (parents, name) = split_parents("src/auth/token.rs");
        assert_eq!(parents.collect::<Vec<_>>(), vec!["src", "auth"]);
        assert_eq!(name, "token.rs");

        let (parents, name) = split_parents("lib.rs");
        assert_eq!(parents.collect::<Vec<_>>(), Vec::<&str>::new());
        assert_eq!(name, "lib.rs");
    }
}

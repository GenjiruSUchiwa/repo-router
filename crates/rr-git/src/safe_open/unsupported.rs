//! Fail-closed opening for targets without a no-follow implementation.
//!
//! Serving source requires proving the bytes came from the entry the snapshot
//! names, reached without following a symlink. Where that proof is not
//! implemented, no content is served: an execution error is the honest answer,
//! and it is louder than a refusal status a caller might read as "expected".

use std::path::Path;

use rr_core::path::RelPath;

use super::OpenOutcome;
use crate::{Error, Result};

/// Placeholder `stat`; uninhabited, because no handle is ever opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryStat {
    never: std::convert::Infallible,
}

impl EntryStat {
    pub(crate) const fn is_regular_file(self) -> bool {
        match self.never {}
    }

    pub(crate) const fn size(self) -> u64 {
        match self.never {}
    }
}

pub(crate) fn open_regular_file(_root: &Path, _rel: &RelPath) -> Result<OpenOutcome> {
    Err(Error::Content(
        "verified source is not supported on this platform: no no-follow open implementation"
            .to_owned(),
    ))
}

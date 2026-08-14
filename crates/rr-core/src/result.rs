use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::index::{FileId, FileRecord, Snapshot, SymbolId, SymbolRecord};
use crate::oid::Oid;
use crate::path::RelPath;
use crate::verify::SourceResult;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "[u32; 2]", try_from = "[u32; 2]")]
pub struct LineRange {
    start: u32,
    end: u32,
}

impl LineRange {
    /// Creates a new 1-based inclusive line range.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSpanLineRange`] if `start < 1` or `start > end`.
    pub fn new(start: u32, end: u32) -> Result<Self> {
        if start < 1 || end < 1 || start > end {
            return Err(Error::InvalidSpanLineRange {
                start_line: start,
                end_line: end,
            });
        }
        Ok(Self { start, end })
    }

    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
}

impl From<LineRange> for [u32; 2] {
    fn from(range: LineRange) -> Self {
        [range.start, range.end]
    }
}

impl TryFrom<[u32; 2]> for LineRange {
    type Error = Error;

    fn try_from(raw: [u32; 2]) -> Result<Self> {
        Self::new(raw[0], raw[1])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pipeline {
    Exact,
    Lexical,
    Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoneReason {
    NotFound,
    LowConfidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetId {
    File(FileId),
    Symbol(SymbolId),
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(into = "f32", try_from = "f32")]
pub struct Confidence(f32);

impl Confidence {
    pub const ONE: Self = Self(1.0);

    /// Creates a new finite confidence score bounded within `0.0..=1.0`.
    ///
    /// # Errors
    /// Returns [`Error::SnapshotInvariant`] if `value` is not finite or out of range.
    pub fn new(value: f32) -> Result<Self> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::SnapshotInvariant {
                reason: "confidence must be finite and within 0.0..=1.0",
            })
        }
    }

    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl From<Confidence> for f32 {
    fn from(conf: Confidence) -> Self {
        conf.0
    }
}

impl TryFrom<f32> for Confidence {
    type Error = Error;

    fn try_from(value: f32) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub target: TargetId,
    pub confidence: Option<Confidence>,
}

impl Candidate {
    #[must_use]
    pub const fn new(target: TargetId, confidence: Option<Confidence>) -> Self {
        Self { target, confidence }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    Direct {
        candidate: Candidate,
        pipeline: Pipeline,
        /// Present only when `--source` asked for it. Candidates and no-match
        /// results carry no source and never trigger content acquisition.
        source: Option<SourceResult>,
    },
    Candidates {
        candidates: SmallVec<[Candidate; 3]>,
        pipeline: Pipeline,
    },
    None {
        reason: NoneReason,
        pipeline: Pipeline,
    },
}

impl QueryResult {
    /// Validates the stable result invariants before rendering.
    ///
    /// # Errors
    /// Returns [`Error::SnapshotInvariant`] when a public result value cannot
    /// be represented by the v1 contract.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Direct { candidate, .. } if candidate.confidence.is_none() => {
                Err(Error::SnapshotInvariant {
                    reason: "direct result is missing confidence",
                })
            }
            Self::Candidates { candidates, .. } if !(1..=3).contains(&candidates.len()) => {
                Err(Error::SnapshotInvariant {
                    reason: "candidate result must contain between one and three candidates",
                })
            }
            _ => Ok(()),
        }
    }

    /// The single mapping from a result to a process exit code.
    ///
    /// An expected source refusal is the only way a direct answer exits
    /// non-zero: the anchor is still correct, but no bytes were released.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Direct {
                source: Some(SourceResult::Refused { .. }),
                ..
            } => 4,
            Self::Direct { .. } => 0,
            Self::Candidates { .. } => 2,
            Self::None { .. } => 3,
        }
    }
}

/// The snapshot records one direct anchor resolves to, with every id and
/// ownership invariant already checked.
///
/// Both anchor rendering and source verification start here so the
/// snapshot-boundary checks exist exactly once.
pub(crate) struct ResolvedTarget<'a> {
    pub(crate) file: &'a FileRecord,
    pub(crate) symbol: Option<&'a SymbolRecord>,
    pub(crate) path: &'a str,
}

pub(crate) fn resolve_target(snapshot: &Snapshot, target: TargetId) -> Result<ResolvedTarget<'_>> {
    let (file, symbol) = match target {
        TargetId::File(file_id) => {
            let file = snapshot
                .files
                .get(file_id.index())
                .ok_or(Error::SnapshotInvariant {
                    reason: "file id out of bounds",
                })?;
            if file.id != file_id {
                return Err(Error::SnapshotInvariant {
                    reason: "file record id disagrees with target id",
                });
            }
            (file, None)
        }
        TargetId::Symbol(symbol_id) => {
            let symbol =
                snapshot
                    .symbols
                    .get(symbol_id.index())
                    .ok_or(Error::SnapshotInvariant {
                        reason: "symbol id out of bounds",
                    })?;
            if symbol.id != symbol_id {
                return Err(Error::SnapshotInvariant {
                    reason: "symbol record id disagrees with target id",
                });
            }
            let file = snapshot
                .files
                .get(symbol.file.index())
                .ok_or(Error::SnapshotInvariant {
                    reason: "symbol file id out of bounds",
                })?;
            let first_symbol = file.first_symbol as usize;
            let symbol_count = file.symbol_count as usize;
            let end_symbol =
                first_symbol
                    .checked_add(symbol_count)
                    .ok_or(Error::SnapshotInvariant {
                        reason: "file symbol range overflow",
                    })?;
            if symbol_id.index() < first_symbol || symbol_id.index() >= end_symbol {
                return Err(Error::SnapshotInvariant {
                    reason: "symbol is not owned by indexed file",
                });
            }
            (file, Some(symbol))
        }
    };

    let path = snapshot
        .strings
        .get(file.path.index())
        .ok_or(Error::SnapshotInvariant {
            reason: "file path string out of bounds",
        })?;
    let _validated = RelPath::new(path)?;
    Ok(ResolvedTarget {
        file,
        symbol,
        path: path.as_str(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorRef<'a> {
    pub path: &'a str,
    pub symbol: Option<&'a str>,
    pub lines: Option<LineRange>,
    pub indexed_oid: Oid,
}

/// Resolves a [`TargetId`] to an [`AnchorRef`] referencing the snapshot.
///
/// # Errors
/// Returns [`Error::SnapshotInvariant`] if the target id or its metadata is invalid.
pub fn resolve_anchor(snapshot: &Snapshot, target: TargetId) -> Result<AnchorRef<'_>> {
    let resolved = resolve_target(snapshot, target)?;
    let Some(symbol) = resolved.symbol else {
        return Ok(AnchorRef {
            path: resolved.path,
            symbol: None,
            lines: None,
            indexed_oid: resolved.file.content_oid,
        });
    };
    let symbol_name =
        snapshot
            .strings
            .get(symbol.name.index())
            .ok_or(Error::SnapshotInvariant {
                reason: "symbol name string out of bounds",
            })?;
    Ok(AnchorRef {
        path: resolved.path,
        symbol: Some(symbol_name.as_str()),
        lines: Some(LineRange::new(
            symbol.span.start_line(),
            symbol.span.end_line(),
        )?),
        indexed_oid: resolved.file.content_oid,
    })
}

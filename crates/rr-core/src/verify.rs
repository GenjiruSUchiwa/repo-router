//! Identity-verified, bounded source packets for one direct query anchor.
//!
//! Source is served only when the content reacquired through `rr-git` still
//! carries the exact identity the snapshot recorded: same
//! [`ContentRepresentation`] and same content OID. Anything else refuses with a
//! status and no bytes. This module never reads a path, never re-hashes, and
//! never defines a second identity: [`AcquiredContent`] is authoritative.
//!
//! The construction order is deliberate and enforced by the types:
//! [`verify_source`] can only produce a [`PendingPacket`], whose content is
//! unreachable until [`finish_source`] consumes it together with the final
//! race check. A caller that skips the final check cannot observe a byte.

use std::ops::Range;

use serde::{Serialize, Serializer};

use crate::content::AcquiredContent;
use crate::facts::Span;
use crate::index::{ContentRepresentation, Snapshot};
use crate::oid::Oid;
use crate::path::RelPath;
use crate::result::{resolve_target, TargetId};
use crate::{Error, Result};

/// Lines of context offered before the anchor.
pub const SOURCE_CONTEXT_BEFORE: u32 = 3;
/// Lines of context offered after the anchor.
pub const SOURCE_CONTEXT_AFTER: u32 = 3;
/// Hard cap on the lines one packet may carry.
pub const MAX_SOURCE_LINES: u32 = 120;
/// Hard cap on the bytes one packet may carry.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
/// Hard cap on the canonical content accepted for verification at all.
pub const MAX_VERIFIED_CONTENT_BYTES: u64 = 16 * 1024 * 1024;

/// Why a worktree path cannot be served as verified source.
///
/// Produced by `rr-git` during acquisition and revalidation; it lives here so
/// the two crates share one vocabulary instead of translating between two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentPathState {
    /// The path no longer exists.
    Missing,
    /// The final entry, or a parent, is a symlink or reparse point.
    Symlink,
    /// The final entry exists but is not a regular file.
    NotRegular,
    /// The canonical content exceeds [`MAX_VERIFIED_CONTENT_BYTES`].
    TooLarge,
    /// The path or its content changed while it was being verified.
    Raced,
}

/// The result of the final check performed immediately before output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revalidation {
    /// The acquisition is still current; content may be released.
    Fresh,
    /// The acquisition is no longer current; no content may be released.
    Refused(ContentPathState),
}

/// The data-only view of an acquisition that verification consumes.
///
/// `rr-git` keeps the platform-specific evidence needed for the final check;
/// verification only ever sees canonical bytes and their identity.
#[derive(Debug, Clone, Copy)]
pub enum AcquiredSource<'a> {
    /// Canonical content was acquired under the no-symlink, confined policy.
    Acquired(&'a AcquiredContent),
    /// Acquisition refused; the state is reported verbatim.
    Refused(ContentPathState),
}

/// The stable status vocabulary shared by the text and JSON contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    /// Content is present and matched the indexed identity.
    Verified,
    /// The file changed since indexing.
    Stale,
    /// The file no longer exists.
    Missing,
    /// The path is a symlink or reparse point.
    Symlink,
    /// The path is not a regular file.
    NotRegular,
    /// The canonical content is larger than verification accepts.
    TooLarge,
    /// One required anchor line is larger than the output byte cap.
    LineTooLong,
    /// The content is current but is not UTF-8 text, so it cannot be served.
    NotText,
    /// The path or content changed during verification.
    Raced,
}

impl SourceStatus {
    /// The one spelling used by every renderer and by JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Symlink => "symlink",
            Self::NotRegular => "not-regular",
            Self::TooLarge => "too-large",
            Self::LineTooLong => "line-too-long",
            Self::NotText => "not-text",
            Self::Raced => "raced",
        }
    }
}

impl From<ContentPathState> for SourceStatus {
    fn from(state: ContentPathState) -> Self {
        match state {
            ContentPathState::Missing => Self::Missing,
            ContentPathState::Symlink => Self::Symlink,
            ContentPathState::NotRegular => Self::NotRegular,
            ContentPathState::TooLarge => Self::TooLarge,
            ContentPathState::Raced => Self::Raced,
        }
    }
}

impl Serialize for SourceStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The snapshot's content identity for one indexed file.
///
/// A view over the stored [`crate::index::FileRecord`], not a second identity:
/// freshness requires both fields to equal the reacquired content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedContentIdentity {
    /// OID of the exact bytes that were parsed and indexed.
    pub content_oid: Oid,
    /// Whether that OID is a Git-canonical or raw local identity.
    pub representation: ContentRepresentation,
}

/// Everything the snapshot says about one direct anchor's source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedSource {
    path: RelPath,
    identity: IndexedContentIdentity,
    span: Option<Span>,
}

impl IndexedSource {
    /// The worktree path to reacquire, validated by the snapshot boundary.
    #[must_use]
    pub const fn path(&self) -> &RelPath {
        &self.path
    }

    /// The identity the reacquired content must match.
    #[must_use]
    pub const fn identity(&self) -> IndexedContentIdentity {
        self.identity
    }

    /// The indexed anchor span, or `None` when the anchor is a whole file.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        self.span
    }
}

/// Resolves the snapshot facts needed to verify one direct anchor's source.
///
/// A symbol anchor carries issue #4's stored [`Span`]; a file anchor has none,
/// and the whole file is its anchor. Both are bounded by the same budgets.
///
/// # Errors
/// Returns [`Error::SnapshotInvariant`] when the target or its metadata is
/// invalid, and [`Error::InvalidRelPath`] when the stored path is not relative.
pub fn resolve_indexed_source(snapshot: &Snapshot, target: TargetId) -> Result<IndexedSource> {
    let resolved = resolve_target(snapshot, target)?;
    Ok(IndexedSource {
        path: RelPath::new(resolved.path)?,
        identity: IndexedContentIdentity {
            content_oid: resolved.file.content_oid,
            representation: resolved.file.representation,
        },
        span: resolved.symbol.map(|symbol| symbol.span),
    })
}

/// A bounded window of verified canonical source.
///
/// Fields are private: the only way to obtain one is [`finish_source`], which
/// requires the final race check to have succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourcePacket {
    status: SourceStatus,
    representation: ContentRepresentation,
    span: Span,
    requested_lines: (u32, u32),
    served_lines: (u32, u32),
    complete: bool,
    context_clipped: bool,
    omitted_anchor_lines: u32,
    omitted_anchor_bytes: u64,
    ends_with_newline: bool,
    content: String,
}

impl SourcePacket {
    /// Always [`SourceStatus::Verified`]; a packet cannot exist otherwise.
    #[must_use]
    pub const fn status(&self) -> SourceStatus {
        self.status
    }

    /// The canonical representation these bytes were served in.
    #[must_use]
    pub const fn representation(&self) -> ContentRepresentation {
        self.representation
    }

    /// The verified anchor span, exactly as indexed.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The window selection set out to produce: the anchor widened by up to
    /// three existing lines each side, or the anchor alone once a budget has
    /// already truncated it, since context is never asked for in that case.
    #[must_use]
    pub const fn requested_lines(&self) -> (u32, u32) {
        self.requested_lines
    }

    /// The line range actually present in [`SourcePacket::content`].
    #[must_use]
    pub const fn served_lines(&self) -> (u32, u32) {
        self.served_lines
    }

    /// True when every line of the anchor is present.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// True when a context line that exists was dropped by a budget.
    ///
    /// Independent of [`SourcePacket::complete`]: a truncated anchor never
    /// reaches the point of asking for context, so it reports truncation alone.
    #[must_use]
    pub const fn context_clipped(&self) -> bool {
        self.context_clipped
    }

    /// Anchor lines the budgets dropped.
    #[must_use]
    pub const fn omitted_anchor_lines(&self) -> u32 {
        self.omitted_anchor_lines
    }

    /// Anchor bytes the budgets dropped.
    #[must_use]
    pub const fn omitted_anchor_bytes(&self) -> u64 {
        self.omitted_anchor_bytes
    }

    /// Whether the served content itself ends with a newline.
    #[must_use]
    pub const fn ends_with_newline(&self) -> bool {
        self.ends_with_newline
    }

    /// The bounded canonical text.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// The source member of a direct query result.
///
/// Serialized untagged: a served packet is its own object, and a refusal is
/// exactly `{"status": ...}` with no content, span, or representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SourceResult {
    /// Verified content within budget.
    Served(SourcePacket),
    /// An expected refusal carrying no content whatsoever.
    Refused {
        /// Why nothing was served.
        status: SourceStatus,
    },
}

impl SourceResult {
    /// The status of either arm, for renderers that treat them uniformly.
    #[must_use]
    pub const fn status(&self) -> SourceStatus {
        match self {
            Self::Served(packet) => packet.status,
            Self::Refused { status } => *status,
        }
    }
}

/// A packet that has passed identity verification but not the final check.
///
/// Carries no public accessor: content cannot be observed before
/// [`finish_source`].
#[derive(Debug)]
pub struct PendingPacket {
    packet: SourcePacket,
}

/// The outcome of [`verify_source`].
#[derive(Debug)]
pub enum PendingSource {
    /// Verification refused; the refusal is final and carries no content.
    Refused(SourceStatus),
    /// A packet is ready for the final race check.
    Pending(PendingPacket),
}

/// Compares the reacquired content against the snapshot and, if they are one
/// identity, builds the bounded packet without releasing it.
///
/// Precedence is fixed: refused acquisition, then representation, then OID,
/// then size, and only then is the content decoded at all. Staleness always
/// wins over format diagnosis, so replaced binary content is reported as
/// `stale` and never decoded, described, or previewed.
///
/// Content that matches the identity but is not UTF-8 text is refused as
/// `not-text` rather than reported as corruption: the snapshot recorded that
/// file correctly, and the caller can still act on the anchor itself.
///
/// # Errors
/// Propagates the span error when a matching identity carries a span that does
/// not fit its own content. That one really is snapshot corruption — the bytes
/// are the indexed bytes, so only the recorded span can be wrong — and it is
/// never staleness.
pub fn verify_source(
    indexed: &IndexedSource,
    acquired: AcquiredSource<'_>,
) -> Result<PendingSource> {
    let current = match acquired {
        AcquiredSource::Refused(state) => return Ok(PendingSource::Refused(state.into())),
        AcquiredSource::Acquired(content) => content,
    };

    if current.representation != indexed.identity.representation
        || current.oid != indexed.identity.content_oid
    {
        return Ok(PendingSource::Refused(SourceStatus::Stale));
    }

    if u64::try_from(current.bytes.len()).unwrap_or(u64::MAX) > MAX_VERIFIED_CONTENT_BYTES {
        return Ok(PendingSource::Refused(SourceStatus::TooLarge));
    }

    let Ok(text) = std::str::from_utf8(&current.bytes) else {
        return Ok(PendingSource::Refused(SourceStatus::NotText));
    };
    if text.as_bytes().contains(&0) {
        return Ok(PendingSource::Refused(SourceStatus::NotText));
    }

    let span = match indexed.span {
        Some(span) => span,
        None => whole_file_span(text)?,
    };
    count_span_validation();
    span.validate_for(text)?;

    let Some(window) = select_window(text, span) else {
        return Ok(PendingSource::Refused(SourceStatus::LineTooLong));
    };

    let selected = text[window.content.clone()].to_owned();
    Ok(PendingSource::Pending(PendingPacket {
        packet: SourcePacket {
            status: SourceStatus::Verified,
            representation: indexed.identity.representation,
            span,
            requested_lines: window.requested_lines,
            served_lines: window.served_lines,
            complete: window.omitted_anchor_lines == 0,
            context_clipped: window.omitted_anchor_lines == 0
                && window.served_lines != window.requested_lines,
            omitted_anchor_lines: window.omitted_anchor_lines,
            omitted_anchor_bytes: window.omitted_anchor_bytes,
            ends_with_newline: selected.ends_with('\n'),
            content: selected,
        },
    }))
}

/// Releases a pending packet only if the final race check reported fresh.
///
/// This is the only constructor of a served [`SourceResult`]. A refused final
/// state drops the pending content unread.
#[must_use]
pub fn finish_source(pending: PendingPacket, final_state: Revalidation) -> SourceResult {
    match final_state {
        Revalidation::Fresh => SourceResult::Served(pending.packet),
        Revalidation::Refused(state) => SourceResult::Refused {
            status: state.into(),
        },
    }
}

/// The whole-file span of a file anchor, which the snapshot does not store.
fn whole_file_span(text: &str) -> Result<Span> {
    let end_byte = u32::try_from(text.len()).map_err(|_| Error::CorruptSource {
        reason: "canonical content is larger than a span can address",
    })?;
    if text.is_empty() {
        return Span::new(0, 0, 1, 1);
    }
    let newlines = text.matches('\n').count();
    let trailing = usize::from(text.ends_with('\n'));
    let end_line = u32::try_from(newlines - trailing + 1).map_err(|_| Error::CorruptSource {
        reason: "canonical content has more lines than a span can address",
    })?;
    Span::new(0, end_byte, 1, end_line)
}

/// The byte range and line arithmetic of one bounded window.
struct SourceWindow {
    content: Range<usize>,
    requested_lines: (u32, u32),
    served_lines: (u32, u32),
    omitted_anchor_lines: u32,
    omitted_anchor_bytes: u64,
}

/// Selects the bounded window for `span`, or `None` when the first anchor line
/// alone exceeds [`MAX_SOURCE_BYTES`] and no whole line can be served.
///
/// The anchor comes first: context is offered only once every anchor line fits,
/// because context around a definition that could not itself be shown would
/// spend the budget on the wrong bytes. Line boundaries are found by scanning
/// outward from the span, so a large file is never walked from line one and no
/// per-line allocation happens. `\n` cannot occur inside a multi-byte scalar,
/// so every boundary here is also a UTF-8 boundary.
fn select_window(text: &str, span: Span) -> Option<SourceWindow> {
    let anchor_first_start = line_start(text, offset_of(span.start_byte()));
    let anchor_last_probe = if span.is_empty() {
        offset_of(span.start_byte())
    } else {
        offset_of(span.end_byte()) - 1
    };
    let anchor_end = line_end(text, line_start(text, anchor_last_probe));

    let mut served_anchor_lines = 0_u32;
    let mut window_end = anchor_first_start;
    while served_anchor_lines < MAX_SOURCE_LINES
        && span.start_line() + served_anchor_lines <= span.end_line()
    {
        let next_end = line_end(text, window_end);
        if next_end - anchor_first_start > MAX_SOURCE_BYTES {
            break;
        }
        window_end = next_end;
        served_anchor_lines += 1;
    }
    if served_anchor_lines == 0 {
        return None;
    }

    let anchor_lines = span.end_line() - span.start_line() + 1;
    let omitted_anchor_lines = anchor_lines - served_anchor_lines;
    let omitted_anchor_bytes = u64::try_from(anchor_end - window_end).unwrap_or(u64::MAX);

    let mut window_start = anchor_first_start;
    let mut first_line = span.start_line();
    let mut last_line = span.start_line() + served_anchor_lines - 1;
    let mut served_lines = served_anchor_lines;

    if omitted_anchor_lines == 0 {
        while first_line > 1
            && first_line + SOURCE_CONTEXT_BEFORE > span.start_line()
            && served_lines < MAX_SOURCE_LINES
        {
            let previous_start = line_start(text, window_start - 1);
            if window_end - previous_start > MAX_SOURCE_BYTES {
                break;
            }
            window_start = previous_start;
            first_line -= 1;
            served_lines += 1;
        }
        while window_end < text.len()
            && last_line < span.end_line() + SOURCE_CONTEXT_AFTER
            && served_lines < MAX_SOURCE_LINES
        {
            let next_end = line_end(text, window_end);
            if next_end - window_start > MAX_SOURCE_BYTES {
                break;
            }
            window_end = next_end;
            last_line += 1;
            served_lines += 1;
        }
    }

    let requested_lines = if omitted_anchor_lines == 0 {
        (
            span.start_line() - SOURCE_CONTEXT_BEFORE.min(span.start_line() - 1),
            span.end_line() + existing_lines_after(text, anchor_end, SOURCE_CONTEXT_AFTER),
        )
    } else {
        (span.start_line(), span.end_line())
    };

    Some(SourceWindow {
        content: window_start..window_end,
        requested_lines,
        served_lines: (first_line, last_line),
        omitted_anchor_lines,
        omitted_anchor_bytes,
    })
}

/// Counts how many of the `wanted` lines after `from` actually exist, which is
/// what makes a requested window "up to three *existing* lines".
fn existing_lines_after(text: &str, from: usize, wanted: u32) -> u32 {
    let mut cursor = from;
    let mut found = 0;
    while found < wanted && cursor < text.len() {
        cursor = line_end(text, cursor);
        found += 1;
    }
    found
}

/// Start offset of the line containing `offset`.
fn line_start(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1)
}

/// Exclusive end offset, newline included, of the line beginning at `start`.
fn line_end(text: &str, start: usize) -> usize {
    text.as_bytes()[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(text.len(), |index| start + index + 1)
}

/// Widens a span offset already validated against the content it addresses.
const fn offset_of(value: u32) -> usize {
    value as usize
}

#[cfg(test)]
thread_local! {
    static SPAN_VALIDATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_span_validation() {
    SPAN_VALIDATIONS.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
const fn count_span_validation() {}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::oid::HashAlgo;

    fn oid(byte: u8) -> Oid {
        Oid::from_raw(&[byte; 20]).unwrap()
    }

    fn indexed(span: Option<Span>) -> IndexedSource {
        IndexedSource {
            path: RelPath::new("src/lib.rs").unwrap(),
            identity: IndexedContentIdentity {
                content_oid: oid(1),
                representation: ContentRepresentation::GitCanonical,
            },
            span,
        }
    }

    fn acquired(text: &str) -> AcquiredContent {
        AcquiredContent {
            oid: oid(1),
            representation: ContentRepresentation::GitCanonical,
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn serve(indexed: &IndexedSource, content: &AcquiredContent) -> SourceResult {
        match verify_source(indexed, AcquiredSource::Acquired(content)).unwrap() {
            PendingSource::Pending(pending) => finish_source(pending, Revalidation::Fresh),
            PendingSource::Refused(status) => SourceResult::Refused { status },
        }
    }

    fn served_packet(result: &SourceResult) -> &SourcePacket {
        match result {
            SourceResult::Served(packet) => packet,
            SourceResult::Refused { status } => panic!("expected served, got {}", status.as_str()),
        }
    }

    fn span_validations() -> u32 {
        SPAN_VALIDATIONS.with(std::cell::Cell::get)
    }

    #[test]
    fn serves_the_definition_and_three_lines_of_context() {
        let text = "a\nb\nc\nd\nDEF\ne\nf\ng\nh\n";

        let source = indexed(Some(Span::new(8, 11, 5, 5).unwrap()));
        let result = serve(&source, &acquired(text));
        let packet = served_packet(&result);
        assert_eq!(packet.status(), SourceStatus::Verified);
        assert_eq!(packet.requested_lines(), (2, 8));
        assert_eq!(packet.served_lines(), (2, 8));
        assert_eq!(packet.content(), "b\nc\nd\nDEF\ne\nf\ng\n");
        assert!(packet.complete());
        assert!(!packet.context_clipped());
        assert!(packet.ends_with_newline());
        assert_eq!(packet.omitted_anchor_lines(), 0);
        assert_eq!(packet.omitted_anchor_bytes(), 0);
    }

    #[test]
    fn representation_mismatch_is_stale_before_span_validation() {
        let before = span_validations();
        let source = indexed(Some(Span::new(0, 1, 1, 1).unwrap()));
        let mut content = acquired("x\n");
        content.representation = ContentRepresentation::RawNoGit;
        assert_eq!(
            serve(&source, &content),
            SourceResult::Refused {
                status: SourceStatus::Stale
            }
        );
        assert_eq!(span_validations(), before, "no span was validated");
    }

    #[test]
    fn oid_mismatch_is_stale_before_utf8_and_span_validation() {
        let before = span_validations();
        let source = indexed(Some(Span::new(0, 1, 1, 1).unwrap()));
        let mut content = acquired("x\n");
        content.oid = oid(2);

        content.bytes = vec![0xff, 0xfe, 0x00];
        assert_eq!(
            serve(&source, &content),
            SourceResult::Refused {
                status: SourceStatus::Stale
            }
        );
        assert_eq!(span_validations(), before, "no span was validated");
    }

    #[test]
    fn refused_acquisition_maps_state_to_status_without_content() {
        let source = indexed(Some(Span::new(0, 1, 1, 1).unwrap()));
        for (state, status) in [
            (ContentPathState::Missing, SourceStatus::Missing),
            (ContentPathState::Symlink, SourceStatus::Symlink),
            (ContentPathState::NotRegular, SourceStatus::NotRegular),
            (ContentPathState::TooLarge, SourceStatus::TooLarge),
            (ContentPathState::Raced, SourceStatus::Raced),
        ] {
            let pending = verify_source(&source, AcquiredSource::Refused(state)).unwrap();
            match pending {
                PendingSource::Refused(actual) => assert_eq!(actual, status),
                PendingSource::Pending(_) => panic!("refusal must not produce a packet"),
            }
        }
    }

    #[test]
    fn matching_identity_with_invalid_span_is_corruption_not_a_clamp() {
        let text = "one\ntwo\n";
        let source = indexed(Some(Span::new(0, 400, 1, 2).unwrap()));
        let error = verify_source(&source, AcquiredSource::Acquired(&acquired(text))).unwrap_err();
        assert!(matches!(error, Error::SpanOutOfBounds { .. }));

        let wrong_line = indexed(Some(Span::new(0, 3, 2, 2).unwrap()));
        let error =
            verify_source(&wrong_line, AcquiredSource::Acquired(&acquired(text))).unwrap_err();
        assert!(matches!(error, Error::SpanLineMismatch));
    }

    #[test]
    fn matching_identity_that_is_not_text_is_refused_not_an_error() {
        let mut content = acquired("");
        content.bytes = vec![b'f', b'n', 0, b'x'];
        let source = indexed(Some(Span::new(0, 4, 1, 1).unwrap()));
        let refused = SourceResult::Refused {
            status: SourceStatus::NotText,
        };
        assert_eq!(serve(&source, &content), refused);

        content.bytes = vec![0xff, 0xff];
        assert_eq!(serve(&source, &content), refused);
    }

    #[test]
    fn exactly_one_hundred_twenty_anchor_lines_are_complete() {
        let text = "x\n".repeat(120);
        let source = indexed(Some(Span::new(0, 240, 1, 120).unwrap()));
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(packet.complete());
        assert_eq!(packet.served_lines(), (1, 120));
        assert_eq!(packet.omitted_anchor_lines(), 0);
        assert_eq!(
            packet.requested_lines(),
            (1, 120),
            "the file has no more lines"
        );
        assert!(!packet.context_clipped());
    }

    #[test]
    fn one_hundred_twenty_one_anchor_lines_truncate_with_exact_counts() {
        let text = "x\n".repeat(121);
        let source = indexed(Some(Span::new(0, 242, 1, 121).unwrap()));
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(!packet.complete());
        assert_eq!(packet.served_lines(), (1, 120));
        assert_eq!(packet.omitted_anchor_lines(), 1);
        assert_eq!(packet.omitted_anchor_bytes(), 2);
        assert_eq!(packet.content().lines().count(), 120);
        assert_eq!(
            packet.requested_lines(),
            (1, 121),
            "the anchor alone, since a truncated anchor never asks for context"
        );
        assert!(
            !packet.context_clipped(),
            "no context line was dropped: none was ever in play"
        );
    }

    #[test]
    fn exactly_sixty_four_kib_is_complete_and_one_byte_more_truncates() {

        let line = format!("{}\n", "y".repeat(32 * 1024 - 1));
        let text = line.repeat(2);
        let end = u32::try_from(text.len()).unwrap();
        let source = indexed(Some(Span::new(0, end, 1, 2).unwrap()));
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(packet.complete());
        assert_eq!(packet.content().len(), MAX_SOURCE_BYTES);

        let text = format!("{text}z\n");
        let end = u32::try_from(text.len()).unwrap();
        let source = indexed(Some(Span::new(0, end, 1, 3).unwrap()));
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(!packet.complete(), "the byte cap wins before the line cap");
        assert_eq!(packet.served_lines(), (1, 2));
        assert_eq!(packet.omitted_anchor_lines(), 1);
        assert_eq!(packet.omitted_anchor_bytes(), 2);
        assert_eq!(packet.content().len(), MAX_SOURCE_BYTES);
    }

    #[test]
    fn a_required_line_over_the_byte_cap_refuses_with_no_content() {
        let text = format!("{}\n", "w".repeat(MAX_SOURCE_BYTES));
        let end = u32::try_from(text.len()).unwrap();
        let source = indexed(Some(Span::new(0, end, 1, 1).unwrap()));
        assert_eq!(
            serve(&source, &acquired(&text)),
            SourceResult::Refused {
                status: SourceStatus::LineTooLong
            }
        );
    }

    #[test]
    fn context_clipping_does_not_make_a_served_anchor_incomplete() {

        let anchor = "v".repeat(MAX_SOURCE_BYTES - 1);
        let text = format!("before\n{anchor}\nafter\n");
        let start = u32::try_from("before\n".len()).unwrap();
        let end = start + u32::try_from(anchor.len()).unwrap();
        let source = indexed(Some(Span::new(start, end, 2, 2).unwrap()));
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(packet.complete());
        assert!(packet.context_clipped());
        assert_eq!(packet.served_lines(), (2, 2));
        assert_eq!(packet.requested_lines(), (1, 3));
        assert_eq!(packet.omitted_anchor_lines(), 0);
    }

    #[test]
    fn unicode_at_span_boundaries_slices_whole_scalars() {
        let text = "let a = \"héllo 🌍\";\nlet b = 2;\n";
        let end = u32::try_from(text.find('\n').unwrap() + 1).unwrap();
        let source = indexed(Some(Span::new(0, end, 1, 1).unwrap()));
        let result = serve(&source, &acquired(text));
        let packet = served_packet(&result);
        assert_eq!(packet.content(), text);
        assert_eq!(packet.served_lines(), (1, 2));
    }

    #[test]
    fn content_without_a_final_newline_reports_it() {
        let text = "fn a() {}";
        let source = indexed(Some(Span::new(0, 9, 1, 1).unwrap()));
        let result = serve(&source, &acquired(text));
        let packet = served_packet(&result);
        assert!(!packet.ends_with_newline());
        assert_eq!(packet.content(), text);
        assert_eq!(packet.served_lines(), (1, 1));
        assert_eq!(packet.requested_lines(), (1, 1));
        assert!(!packet.context_clipped());
    }

    #[test]
    fn an_empty_span_in_an_empty_file_serves_nothing_without_panicking() {
        let source = indexed(Some(Span::new(0, 0, 1, 1).unwrap()));
        let result = serve(&source, &acquired(""));
        let packet = served_packet(&result);
        assert_eq!(packet.content(), "");
        assert!(!packet.ends_with_newline());
        assert_eq!(packet.served_lines(), (1, 1));
    }

    #[test]
    fn an_empty_span_after_a_final_newline_serves_the_trailing_line() {
        let text = "a\n";
        let source = indexed(Some(Span::new(2, 2, 2, 2).unwrap()));
        let result = serve(&source, &acquired(text));
        let packet = served_packet(&result);
        assert_eq!(packet.content(), "a\n");
        assert_eq!(packet.served_lines(), (1, 2));
        assert_eq!(packet.requested_lines(), (1, 2));
    }

    #[test]
    fn a_file_anchor_serves_the_whole_file_within_budget() {
        let text = "one\ntwo\nthree\n";
        let source = indexed(None);
        let result = serve(&source, &acquired(text));
        let packet = served_packet(&result);
        assert_eq!(packet.span(), Span::new(0, 14, 1, 3).unwrap());
        assert_eq!(packet.content(), text);
        assert!(packet.complete());
        assert_eq!(packet.served_lines(), (1, 3));
    }

    #[test]
    fn a_file_anchor_larger_than_the_line_budget_truncates() {
        let text = "l\n".repeat(200);
        let source = indexed(None);
        let result = serve(&source, &acquired(&text));
        let packet = served_packet(&result);
        assert!(!packet.complete());
        assert_eq!(packet.served_lines(), (1, 120));
        assert_eq!(packet.omitted_anchor_lines(), 80);
        assert_eq!(packet.omitted_anchor_bytes(), 160);
    }

    #[test]
    fn content_over_the_verification_cap_is_refused_as_too_large() {
        let mut content = acquired("");
        content.bytes = vec![b'x'; usize::try_from(MAX_VERIFIED_CONTENT_BYTES).unwrap() + 1];
        let source = indexed(Some(Span::new(0, 1, 1, 1).unwrap()));
        let pending = verify_source(&source, AcquiredSource::Acquired(&content)).unwrap();
        match pending {
            PendingSource::Refused(status) => assert_eq!(status, SourceStatus::TooLarge),
            PendingSource::Pending(_) => panic!("oversized content must not be packetized"),
        }
    }

    #[test]
    fn a_refused_final_check_drops_pending_content() {
        let source = indexed(Some(Span::new(0, 2, 1, 1).unwrap()));
        let content = acquired("x\n");
        let PendingSource::Pending(pending) =
            verify_source(&source, AcquiredSource::Acquired(&content)).unwrap()
        else {
            panic!("expected a pending packet");
        };
        let result = finish_source(pending, Revalidation::Refused(ContentPathState::Raced));
        assert_eq!(
            result,
            SourceResult::Refused {
                status: SourceStatus::Raced
            }
        );
        let rendered = serde_json::to_string(&result).unwrap();
        assert_eq!(rendered, r#"{"status":"raced"}"#);
    }

    #[test]
    fn every_refusal_serializes_without_content_or_representation() {
        for status in [
            SourceStatus::Stale,
            SourceStatus::Missing,
            SourceStatus::Symlink,
            SourceStatus::NotRegular,
            SourceStatus::TooLarge,
            SourceStatus::LineTooLong,
            SourceStatus::Raced,
        ] {
            let rendered = serde_json::to_string(&SourceResult::Refused { status }).unwrap();
            assert_eq!(rendered, format!(r#"{{"status":"{}"}}"#, status.as_str()));
        }
    }

    #[test]
    fn a_served_packet_never_serializes_a_content_oid() {
        let text = "fn a() {}\n";
        let source = indexed(Some(Span::new(0, 10, 1, 1).unwrap()));
        let result = serve(&source, &acquired(text));
        let rendered = serde_json::to_string(&result).unwrap();
        assert!(!rendered.contains("oid"), "{rendered}");
        assert!(rendered.starts_with(r#"{"status":"verified","representation":"git-canonical","span":{"start_byte":0,"end_byte":10,"start_line":1,"end_line":1}"#), "{rendered}");
        assert!(
            rendered.contains(r#""requested_lines":[1,1],"served_lines":[1,1]"#),
            "{rendered}"
        );
    }

    #[test]
    fn the_span_is_validated_exactly_once_per_verification() {
        let before = span_validations();
        let source = indexed(Some(Span::new(0, 2, 1, 1).unwrap()));
        let _ = serve(&source, &acquired("x\n"));
        assert_eq!(span_validations(), before + 1);
    }

    #[test]
    fn oid_algorithms_are_compared_as_whole_identities() {

        let sha256 = Oid::from_raw(&[1_u8; 32]).unwrap();
        assert_eq!(sha256.algo(), HashAlgo::Sha256);
        let source = indexed(Some(Span::new(0, 2, 1, 1).unwrap()));
        let mut content = acquired("x\n");
        content.oid = sha256;
        assert_eq!(
            serve(&source, &content),
            SourceResult::Refused {
                status: SourceStatus::Stale
            }
        );
    }
}

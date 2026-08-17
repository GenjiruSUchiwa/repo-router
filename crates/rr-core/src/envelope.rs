//! The framing every rebuildable rr file on disk shares.
//!
//! Two files use it today — the published snapshot
//! (`crates/rr-core/src/snapshot.rs`) and the co-change cache
//! (`crates/rr-git/src/cochange.rs`) — and one module owns it, because the
//! layout is a format and not a convention. Two hand-written copies of one
//! format drift in the direction no test catches: each round-trips its own
//! bytes, so both suites stay green while the two files quietly stop being the
//! same shape.
//!
//! # Every file states its own magic
//!
//! Sharing one magic across formats and telling them apart by the version word
//! looks safe while the two version numbers differ, and stops being safe the
//! day they collide — one file then passes the other's magic *and* version
//! check, and is refused only by the payload decoder, which reports corruption
//! for a file that is not corrupt. `wrap` and `unwrap` therefore take the magic
//! from the caller, so two formats can never occupy one namespace.

/// Magic, version word, payload length, BLAKE3 checksum over the payload.
pub const HEADER_LEN: usize = 8 + 4 + 8 + 32;

/// End of the magic, and the start of the version word.
const MAGIC_END: usize = 8;
/// End of the `u32` version word.
const VERSION_END: usize = MAGIC_END + 4;
/// End of the `u64` payload length, and the start of the checksum.
const LENGTH_END: usize = VERSION_END + 8;

/// What one framed file turned out to hold.
///
/// Short and long are separate answers rather than one "the length disagrees",
/// because a caller that repairs by rebuilding does not care while a caller that
/// *reports* does: trailing bytes mean something appended to a complete file,
/// and a short read means something truncated an incomplete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing<'bytes> {
    /// The payload, checksum-verified and exactly as long as the header claims.
    Payload(&'bytes [u8]),
    /// Not this format's magic.
    BadMagic,
    /// This format, written by a binary that framed it differently.
    UnsupportedVersion {
        /// The version word the file carries.
        found: u32,
    },
    /// Fewer bytes than the header needs, or than it claims.
    LengthMismatch,
    /// A complete file with bytes after it.
    TrailingBytes,
    /// The payload is not what the checksum was taken over.
    ChecksumMismatch,
}

/// Wraps one payload, or `None` if its length does not fit the header.
#[must_use]
pub fn wrap(magic: [u8; 8], version: u32, payload: &[u8]) -> Option<Vec<u8>> {
    let length = u64::try_from(payload.len()).ok()?;
    let mut bytes = Vec::with_capacity(HEADER_LEN.checked_add(payload.len())?);
    bytes.extend_from_slice(&magic);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(blake3::hash(payload).as_bytes());
    bytes.extend_from_slice(payload);
    Some(bytes)
}

/// Reads back what [`wrap`] wrote, or says which agreement failed.
///
/// The length is settled before the checksum is consulted, because a digest
/// taken over bytes of unknown extent proves nothing about the file it came
/// from.
#[must_use]
pub fn unwrap(magic: [u8; 8], version: u32, bytes: &[u8]) -> Framing<'_> {
    if bytes.len() < HEADER_LEN {
        return Framing::LengthMismatch;
    }
    if bytes[..MAGIC_END] != magic {
        return Framing::BadMagic;
    }
    let found = u32::from_le_bytes(word(&bytes[MAGIC_END..VERSION_END]));
    if found != version {
        return Framing::UnsupportedVersion { found };
    }
    let claimed = u64::from_le_bytes(long(&bytes[VERSION_END..LENGTH_END]));
    let Ok(claimed) = usize::try_from(claimed) else {
        return Framing::LengthMismatch;
    };
    let Some(end) = HEADER_LEN.checked_add(claimed) else {
        return Framing::LengthMismatch;
    };
    if bytes.len() < end {
        return Framing::LengthMismatch;
    }
    if bytes.len() > end {
        return Framing::TrailingBytes;
    }
    let payload = &bytes[HEADER_LEN..];
    if blake3::hash(payload).as_bytes() != &bytes[LENGTH_END..HEADER_LEN] {
        return Framing::ChecksumMismatch;
    }
    Framing::Payload(payload)
}

/// The four bytes of a version word, from a slice already known to hold four.
///
/// A total function rather than a `try_into` at the call site: the length was
/// settled above, and an `unwrap_or` there would put a default version into a
/// comparison that decides whether a file is readable.
fn word(bytes: &[u8]) -> [u8; 4] {
    let mut word = [0; 4];
    word.copy_from_slice(bytes);
    word
}

/// The eight bytes of a length, from a slice already known to hold eight.
fn long(bytes: &[u8]) -> [u8; 8] {
    let mut long = [0; 8];
    long.copy_from_slice(bytes);
    long
}

#[cfg(test)]
mod tests {
    use super::{unwrap, wrap, Framing, HEADER_LEN};

    const MAGIC: [u8; 8] = *b"RRTEST\0\0";
    const OTHER: [u8; 8] = *b"RROTHR\0\0";

    #[test]
    fn a_wrapped_payload_reads_back_verbatim() {
        let wrapped = wrap(MAGIC, 3, b"payload").expect("a short payload fits");
        assert_eq!(wrapped.len(), HEADER_LEN + 7);
        assert_eq!(unwrap(MAGIC, 3, &wrapped), Framing::Payload(b"payload"));
    }

    #[test]
    fn an_empty_payload_is_a_payload_and_not_a_short_file() {
        let wrapped = wrap(MAGIC, 1, b"").expect("an empty payload fits");
        assert_eq!(unwrap(MAGIC, 1, &wrapped), Framing::Payload(b""));
    }

    #[test]
    fn another_format_is_refused_on_the_magic_and_never_on_the_version() {
        let wrapped = wrap(OTHER, 1, b"payload").expect("a short payload fits");
        assert_eq!(unwrap(MAGIC, 1, &wrapped), Framing::BadMagic);
    }

    #[test]
    fn one_magic_shared_by_two_versions_is_still_two_formats() {
        let wrapped = wrap(MAGIC, 12, b"payload").expect("a short payload fits");
        assert_eq!(
            unwrap(MAGIC, 1, &wrapped),
            Framing::UnsupportedVersion { found: 12 }
        );
    }

    #[test]
    fn a_truncated_file_is_short_and_an_extended_one_is_trailing() {
        let wrapped = wrap(MAGIC, 1, b"payload").expect("a short payload fits");

        let mut short = wrapped.clone();
        short.pop();
        assert_eq!(unwrap(MAGIC, 1, &short), Framing::LengthMismatch);

        let mut long = wrapped;
        long.push(0);
        assert_eq!(unwrap(MAGIC, 1, &long), Framing::TrailingBytes);
    }

    #[test]
    fn a_header_alone_is_a_length_mismatch_not_a_bad_magic() {
        assert_eq!(unwrap(MAGIC, 1, &[]), Framing::LengthMismatch);
        assert_eq!(
            unwrap(MAGIC, 1, &[0; HEADER_LEN - 1]),
            Framing::LengthMismatch
        );
    }

    #[test]
    fn an_edited_payload_fails_the_checksum() {
        let mut wrapped = wrap(MAGIC, 1, b"payload").expect("a short payload fits");
        let last = wrapped.len() - 1;
        wrapped[last] ^= 0xff;
        assert_eq!(unwrap(MAGIC, 1, &wrapped), Framing::ChecksumMismatch);
    }
}

//! The one way this crate turns structure into a stable name.
//!
//! Every digest here is built from an explicit stream rather than from a
//! `Debug` rendering, a serialization, or a hash-map traversal. Two builds of
//! the same repository on different machines have to agree on these values, and
//! the only way to guarantee that is to write the bytes out by hand.

use std::fmt;

use super::{TextError, TextResult, TEXT_FORMAT_VERSION};

/// Digests are always published whole. The prefix names the algorithm so a
/// later one can be introduced without a reader having to guess from length.
const DIGEST_PREFIX: &str = "blake3:";
const HEX_DIGITS: usize = 64;

/// A full 32-byte BLAKE3 digest.
///
/// Truncation is not offered, not even privately: a shortened digest is a
/// different security claim, and offering both spellings is how the wrong one
/// ends up in a file that outlives the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    /// The digest of a byte string, for content that is not a hash stream.
    ///
    /// The stream carries length delimiters and a domain separator because it
    /// hashes *structure*, and a structure hashed without them can collide across
    /// field boundaries. A whole file has no fields, so it needs neither.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// The digest as `blake3:` plus 64 lowercase hexadecimal digits.
    #[must_use]
    pub fn to_text(self) -> String {
        let mut out = String::with_capacity(DIGEST_PREFIX.len() + HEX_DIGITS);
        out.push_str(DIGEST_PREFIX);
        for byte in self.0 {
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0F));
        }
        out
    }

    /// Parses the published spelling, rejecting every other one.
    ///
    /// Uppercase hexadecimal, a missing prefix, and a truncated digest are all
    /// refused rather than normalized: a file that says something this crate
    /// would not have written is a file this crate does not own.
    ///
    /// # Errors
    /// Returns [`TextError::Frontmatter`] when the spelling is not canonical.
    pub fn parse(text: &str) -> TextResult<Self> {
        let hex = text
            .strip_prefix(DIGEST_PREFIX)
            .ok_or(TextError::Frontmatter {
                reason: "digest is missing the blake3: prefix",
            })?;
        if hex.len() != HEX_DIGITS {
            return Err(TextError::Frontmatter {
                reason: "digest is not 64 hexadecimal digits",
            });
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_value(pair[0]).ok_or(TextError::Frontmatter {
                reason: "digest contains a non-lowercase-hexadecimal digit",
            })?;
            let low = hex_value(pair[1]).ok_or(TextError::Frontmatter {
                reason: "digest contains a non-lowercase-hexadecimal digit",
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

const fn hex_digit(nibble: u8) -> char {
    (match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }) as char
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// The identity of one directory scope's public API surface.
///
/// A separate type from [`Digest`] because it is the key issue #12 stores
/// against a route: handing out a bare digest would let a caller store the
/// wrong one and never find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiHash(Digest);

impl ApiHash {
    pub(crate) const fn new(digest: Digest) -> Self {
        Self(digest)
    }

    /// The digest behind this key.
    #[must_use]
    pub const fn digest(self) -> Digest {
        self.0
    }

    /// The published spelling.
    #[must_use]
    pub fn to_text(self) -> String {
        self.0.to_text()
    }
}

impl fmt::Display for ApiHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A length-delimited hash stream.
///
/// Every variable-length field is written as an unsigned little-endian length
/// followed by its raw bytes, so no arrangement of field contents can produce
/// the byte sequence of a different arrangement. Fixed-width numbers carry
/// their own length and are written directly.
///
/// The stream opens with a format-version prefix and a domain label, so the
/// same fields hashed for two different purposes cannot collide either.
pub(crate) struct HashStream(blake3::Hasher);

impl HashStream {
    pub(crate) fn new(domain: &'static str) -> Self {
        Self::with_format(domain, TEXT_FORMAT_VERSION)
    }

    pub(crate) fn with_format(domain: &'static str, format: u32) -> Self {
        let mut stream = Self(blake3::Hasher::new());
        stream.0.update(b"rr-text\0");
        stream.0.update(&format.to_le_bytes());
        stream.text(domain);
        stream
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        let length = value.len() as u64;
        self.0.update(&length.to_le_bytes());
        self.0.update(value);
    }

    pub(crate) fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.0.update(&value.to_le_bytes());
    }

    /// Writes how many records follow.
    ///
    /// Saturating rather than fallible: a repository with four billion records
    /// in one list has already failed somewhere this crate can report better,
    /// and a hash that refuses to be computed helps nobody.
    pub(crate) fn count(&mut self, value: usize) {
        self.u32(u32::try_from(value).unwrap_or(u32::MAX));
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    pub(crate) fn digest(&mut self, value: Digest) {
        self.0.update(&value.0);
    }

    pub(crate) fn finish(self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_round_trips_through_its_published_spelling() {
        let digest = HashStream::new("test").finish();
        assert_eq!(Digest::parse(&digest.to_text()).unwrap(), digest);
        assert_eq!(digest.to_text().len(), DIGEST_PREFIX.len() + HEX_DIGITS);
    }

    #[test]
    fn only_the_canonical_spelling_parses() {
        let text = HashStream::new("test").finish().to_text();
        assert!(Digest::parse(&text.to_uppercase()).is_err());
        assert!(Digest::parse(text.trim_start_matches("blake3:")).is_err());
        assert!(Digest::parse(&text[..text.len() - 1]).is_err());
        assert!(Digest::parse(&format!("{text}0")).is_err());
    }

    /// Concatenation is the classic way two different field lists reach the
    /// same bytes. The length prefix is what rules it out, and this is the
    /// adversarial pair it has to separate.
    #[test]
    fn field_boundaries_cannot_be_forged_by_concatenation() {
        let mut left = HashStream::new("d");
        left.text("ab");
        left.text("c");

        let mut right = HashStream::new("d");
        right.text("a");
        right.text("bc");

        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn the_domain_separates_identical_field_lists() {
        let mut left = HashStream::new("one");
        left.text("same");
        let mut right = HashStream::new("two");
        right.text("same");

        assert_ne!(left.finish(), right.finish());
    }

    #[test]
    fn a_byte_digest_is_not_a_stream_digest() {
        let mut stream = HashStream::new("test");
        stream.bytes(b"x");
        assert_ne!(Digest::of_bytes(b"x"), stream.finish());
    }
}

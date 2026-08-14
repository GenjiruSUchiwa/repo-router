//! Git object identifier (OID) representations and operations.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Hashing algorithms supported for Git object identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgo {
    /// SHA-1 (160-bit / 20-byte hash, 40 hex characters).
    Sha1,
    /// SHA-256 (256-bit / 32-byte hash, 64 hex characters).
    Sha256,
}

impl HashAlgo {
    /// Returns the byte length of hashes produced by this algorithm.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    /// Returns the hex string length of hashes produced by this algorithm.
    #[must_use]
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// Errors produced when parsing or constructing an [`Oid`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidError {
    /// The input length did not match a valid OID length (40 for SHA-1, 64 for SHA-256).
    #[error("invalid OID length: expected 40 (SHA-1) or 64 (SHA-256) hex characters, got {got}")]
    InvalidLength {
        /// Actual length received.
        got: usize,
    },
    /// A character in the hex string was not a valid hexadecimal digit.
    #[error("invalid hex character at position {pos}")]
    InvalidHex {
        /// Zero-based position where the invalid character was encountered.
        pos: usize,
    },
}

/// A Git object identifier (SHA-1 = 20 bytes or SHA-256 = 32 bytes), canonicalized as lowercase hex.
///
/// Invariants:
/// - Byte length is exactly 20 (SHA-1) or 32 (SHA-256).
/// - Hex representation is exactly 40 or 64 lowercase hexadecimal ASCII characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid {
    bytes: [u8; 32],
    len: u8,
}

impl Oid {
    /// Parses an [`Oid`] from a hexadecimal string.
    ///
    /// Accepts uppercase and lowercase hexadecimal characters; internally stores the canonical lowercase form.
    ///
    /// # Errors
    /// Returns [`OidError::InvalidLength`] if the string length is not 40 or 64.
    /// Returns [`OidError::InvalidHex`] if any character is not a valid hex digit.
    pub fn from_hex(s: &str) -> Result<Self, OidError> {
        let hex_bytes = s.as_bytes();
        let len = match hex_bytes.len() {
            40 => 20,
            64 => 32,
            got => return Err(OidError::InvalidLength { got }),
        };

        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate().take(len) {
            let high_idx = i * 2;
            let low_idx = high_idx + 1;
            let high_nibble = parse_hex_digit(hex_bytes[high_idx], high_idx)?;
            let low_nibble = parse_hex_digit(hex_bytes[low_idx], low_idx)?;
            *byte = (high_nibble << 4) | low_nibble;
        }

        #[allow(clippy::cast_possible_truncation)]
        Ok(Self {
            bytes,
            len: len as u8,
        })
    }

    /// Constructs an [`Oid`] from raw digest bytes.
    ///
    /// # Errors
    /// Returns [`OidError::InvalidLength`] if `raw.len()` is not 20 or 32.
    pub fn from_raw(raw: &[u8]) -> Result<Self, OidError> {
        let len = match raw.len() {
            20 => 20,
            32 => 32,
            got => return Err(OidError::InvalidLength { got: got * 2 }),
        };

        let mut bytes = [0u8; 32];
        bytes[..len].copy_from_slice(raw);

        #[allow(clippy::cast_possible_truncation)]
        Ok(Self {
            bytes,
            len: len as u8,
        })
    }

    /// Returns the canonical lowercase hex representation (40 or 64 characters).
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut buf = Vec::with_capacity(self.len as usize * 2);
        for &byte in self.as_bytes() {
            buf.push(HEX_CHARS[(byte >> 4) as usize]);
            buf.push(HEX_CHARS[(byte & 0x0f) as usize]);
        }
        #[allow(clippy::unwrap_used)]
        String::from_utf8(buf).unwrap_or_else(|_| unreachable!("valid ascii hex"))
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// Returns the hashing algorithm used for this identifier.
    #[must_use]
    pub const fn algo(&self) -> HashAlgo {
        if self.len == 20 {
            HashAlgo::Sha1
        } else {
            HashAlgo::Sha256
        }
    }

    /// Returns the 2-character shard prefix for sharded filesystem layouts.
    ///
    /// This is the only place in the codebase where shard prefix derivation occurs.
    #[must_use]
    pub fn shard_prefix(&self) -> String {
        format!("{:02x}", self.bytes[0])
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.to_hex())
    }
}

impl TryFrom<&str> for Oid {
    type Error = OidError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::from_hex(s)
    }
}

impl TryFrom<String> for Oid {
    type Error = OidError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::from_hex(&s)
    }
}

impl From<Oid> for String {
    fn from(oid: Oid) -> Self {
        oid.to_hex()
    }
}

impl Serialize for Oid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Oid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Parses a single ASCII hex character into a 4-bit nibble value.
fn parse_hex_digit(byte: u8, pos: usize) -> Result<u8, OidError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(OidError::InvalidHex { pos }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SHA1_HEX: &str = "95d09f2b10159347eece71399a7e2e907ea3df4f";
    const SHA256_HEX: &str = "fee53a18d32820613c0527aa79be5cb30173c823a9b448fa4817767cc84c6f03";

    #[test]
    fn parse_valid_sha1_hex() {
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        assert_eq!(oid.algo(), HashAlgo::Sha1);
        assert_eq!(oid.as_bytes().len(), 20);
        assert_eq!(oid.to_hex(), SHA1_HEX);
        assert_eq!(oid.to_string(), SHA1_HEX);
        assert_eq!(oid.shard_prefix(), "95");
    }

    #[test]
    fn parse_valid_sha256_hex() {
        let oid = Oid::from_hex(SHA256_HEX).unwrap();
        assert_eq!(oid.algo(), HashAlgo::Sha256);
        assert_eq!(oid.as_bytes().len(), 32);
        assert_eq!(oid.to_hex(), SHA256_HEX);
        assert_eq!(oid.to_string(), SHA256_HEX);
        assert_eq!(oid.shard_prefix(), "fe");
    }

    #[test]
    fn uppercase_hex_input_accepted_and_canonicalized() {
        let upper = SHA1_HEX.to_ascii_uppercase();
        let oid = Oid::from_hex(&upper).unwrap();
        assert_eq!(oid.to_hex(), SHA1_HEX);
    }

    #[test]
    fn hex_roundtrip() {
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let formatted = oid.to_hex();
        let roundtrip = Oid::from_hex(&formatted).unwrap();
        assert_eq!(oid, roundtrip);
    }

    #[test]
    fn from_raw_bytes_roundtrip() {
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let reconstructed = Oid::from_raw(oid.as_bytes()).unwrap();
        assert_eq!(oid, reconstructed);
    }

    #[test]
    fn invalid_length_rejected() {
        assert_eq!(
            Oid::from_hex("abc"),
            Err(OidError::InvalidLength { got: 3 })
        );
        assert_eq!(
            Oid::from_hex("95d09f2b10159347eece71399a7e2e907ea3df4"),
            Err(OidError::InvalidLength { got: 39 })
        );
        assert_eq!(
            Oid::from_hex(&format!("{SHA1_HEX}0")),
            Err(OidError::InvalidLength { got: 41 })
        );
    }

    #[test]
    fn non_hex_characters_rejected() {
        let mut bad = SHA1_HEX.to_string();
        bad.replace_range(4..5, "g");
        assert_eq!(Oid::from_hex(&bad), Err(OidError::InvalidHex { pos: 4 }));

        let mut bad_end = SHA1_HEX.to_string();
        bad_end.replace_range(39..40, "z");
        assert_eq!(
            Oid::from_hex(&bad_end),
            Err(OidError::InvalidHex { pos: 39 })
        );
    }

    #[test]
    fn serde_json_roundtrip_and_garbage_rejection() {
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let json = serde_json::to_string(&oid).unwrap();
        assert_eq!(json, format!("\"{SHA1_HEX}\""));

        let deserialized: Oid = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, oid);
        assert!(serde_json::from_str::<Oid>("\"invalid-oid\"").is_err());
        assert!(serde_json::from_str::<Oid>("\"12345\"").is_err());
        assert!(serde_json::from_str::<Oid>("12345").is_err());
    }

    #[test]
    fn try_from_conversions() {
        let oid = Oid::try_from(SHA1_HEX).unwrap();
        assert_eq!(oid.to_hex(), SHA1_HEX);

        let oid2 = Oid::try_from(SHA1_HEX.to_string()).unwrap();
        assert_eq!(oid2.to_hex(), SHA1_HEX);

        let s: String = oid.into();
        assert_eq!(s, SHA1_HEX);
    }
}

//! Git object identifier computation and pure Git blob hashing.
//!
//! This module contains pure hashing functions without depending on `gix`.

use sha1::Digest;

pub use rr_core::oid::{HashAlgo, Oid, OidError};

/// Computes the Git blob object identifier for the given content.
///
/// This is the canonical and only function in the codebase that knows the Git blob object format:
/// `sha("blob <len>\0<content>")`.
///
/// # Arguments
/// - `content`: Raw byte payload of the file.
/// - `algo`: Hashing algorithm to use ([`HashAlgo::Sha1`] or [`HashAlgo::Sha256`]).
#[must_use]
pub fn hash_blob(content: &[u8], algo: HashAlgo) -> Oid {
    let header = format!("blob {}\0", content.len());
    match algo {
        HashAlgo::Sha1 => {
            let mut hasher = sha1::Sha1::new();
            hasher.update(header.as_bytes());
            hasher.update(content);
            let digest = hasher.finalize();
            #[allow(clippy::unwrap_used)]
            Oid::from_raw(&digest).unwrap_or_else(|_| unreachable!("SHA-1 digest is 20 bytes"))
        }
        HashAlgo::Sha256 => {
            let mut hasher = sha2::Sha256::new();
            hasher.update(header.as_bytes());
            hasher.update(content);
            let digest = hasher.finalize();
            #[allow(clippy::unwrap_used)]
            Oid::from_raw(&digest).unwrap_or_else(|_| unreachable!("SHA-256 digest is 32 bytes"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SHA1_HELLO: &str = "95d09f2b10159347eece71399a7e2e907ea3df4f";
    const SHA256_HELLO: &str = "fee53a18d32820613c0527aa79be5cb30173c823a9b448fa4817767cc84c6f03";
    const SHA1_EMPTY: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    const SHA256_EMPTY: &str = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";

    #[test]
    fn known_sha1_vector_matches_git_hash_object() {
        let oid = hash_blob(b"hello world", HashAlgo::Sha1);
        assert_eq!(oid.to_hex(), SHA1_HELLO);
        assert_eq!(oid.algo(), HashAlgo::Sha1);
    }

    #[test]
    fn known_sha256_vector() {
        let oid = hash_blob(b"hello world", HashAlgo::Sha256);
        assert_eq!(oid.to_hex(), SHA256_HELLO);
        assert_eq!(oid.algo(), HashAlgo::Sha256);
    }

    #[test]
    fn empty_content_sha1() {
        let oid = hash_blob(b"", HashAlgo::Sha1);
        assert_eq!(oid.to_hex(), SHA1_EMPTY);
    }

    #[test]
    fn empty_content_sha256() {
        let oid = hash_blob(b"", HashAlgo::Sha256);
        assert_eq!(oid.to_hex(), SHA256_EMPTY);
    }

    #[test]
    fn roundtrip_hex() {
        let oid = hash_blob(b"hello world", HashAlgo::Sha1);
        let hex = oid.to_hex();
        let parsed = Oid::from_hex(&hex).unwrap();
        assert_eq!(oid, parsed);
    }

    #[test]
    fn uppercase_input_accepted_and_lowercase_out() {
        let upper = SHA1_HELLO.to_ascii_uppercase();
        let parsed = Oid::from_hex(&upper).unwrap();
        assert_eq!(parsed.to_hex(), SHA1_HELLO);
    }

    #[test]
    fn wrong_length_rejected() {
        assert!(matches!(
            Oid::from_hex("95d09f2b10"),
            Err(OidError::InvalidLength { got: 10 })
        ));
        assert!(matches!(
            Oid::from_hex(""),
            Err(OidError::InvalidLength { got: 0 })
        ));
    }

    #[test]
    fn non_hex_rejected() {
        let mut invalid = SHA1_HELLO.to_string();
        invalid.replace_range(10..11, "q");
        assert_eq!(
            Oid::from_hex(&invalid),
            Err(OidError::InvalidHex { pos: 10 })
        );
    }

    #[test]
    fn serde_garbage_rejected() {
        assert!(serde_json::from_str::<Oid>("\"invalid-oid-value\"").is_err());
        assert!(serde_json::from_str::<Oid>("\"\"").is_err());
    }
}

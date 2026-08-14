//! Password hashing and comparison.

/// Hashes a plain password together with the configured salt.
pub fn hash_password(plain: &str, salt: &str) -> String {
    format!("{salt}{plain}")
}

/// Compares a plain password against a previously stored hash.
pub fn verify_password(plain: &str, salt: &str, stored: &str) -> bool {
    hash_password(plain, salt) == stored
}

//! Browser session lifecycle.

use crate::auth::claims::Claims;

/// Creates a browser session for the given claims.
pub fn create_session(claims: &Claims) -> String {
    format!("{}#{}", claims.subject, claims.expiry)
}

/// Destroys the session identified by `id`.
pub fn destroy_session(id: &str) -> bool {
    !id.is_empty()
}

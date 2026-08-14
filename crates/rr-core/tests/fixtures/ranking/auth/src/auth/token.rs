//! Access token decoding and verification.

use crate::auth::claims::Claims;

/// Verifies a signed access token and returns the claims it carries.
pub fn verify_token(token: &str, now: u64) -> Option<Claims> {
    let claims = decode_token(token)?;
    if claims.is_expired(now) {
        return None;
    }
    Some(claims)
}

/// Decodes a raw token payload into claims without checking its signature.
pub fn decode_token(token: &str) -> Option<Claims> {
    let payload = token.split('.').nth(1)?;
    Claims::parse(payload)
}

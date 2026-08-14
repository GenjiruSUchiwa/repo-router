//! Claims asserted by a decoded token.

/// The subject and expiry a token asserts.
pub struct Claims {
    /// Identifier of the account the token was issued for.
    pub subject: String,
    /// Unix timestamp after which the token stops being valid.
    pub expiry: u64,
}

impl Claims {
    /// Parses claims from a raw token payload.
    pub fn parse(payload: &str) -> Option<Self> {
        let (subject, expiry) = payload.split_once(':')?;
        Some(Self {
            subject: subject.to_string(),
            expiry: expiry.parse().ok()?,
        })
    }

    /// Reports whether the claims expired before `now`.
    pub fn is_expired(&self, now: u64) -> bool {
        self.expiry < now
    }
}

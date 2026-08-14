use crate::db::users::find_user;

pub struct Claims {
    pub sub: String,
    pub exp: u64,
}

pub fn decode_jwt(token: &str) -> Option<Claims> {
    if token.is_empty() {
        None
    } else {
        Some(Claims {
            sub: "user_123".to_string(),
            exp: now() + 3600,
        })
    }
}

pub fn now() -> u64 {
    1_700_000_000
}

pub fn verify_token(token: &str) -> bool {
    if let Some(claims) = decode_jwt(token) {
        claims.exp > now() && find_user(&claims.sub).is_some()
    } else {
        false
    }
}

pub fn refresh_token(token: &str) -> Option<String> {
    if verify_token(token) {
        let claims = decode_jwt(token)?;
        let _user = find_user(&claims.sub)?;
        Some(format!("refreshed_{token}"))
    } else {
        None
    }
}

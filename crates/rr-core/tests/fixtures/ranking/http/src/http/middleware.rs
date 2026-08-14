//! Layers that run before a request reaches its handler.

/// Applies every registered middleware to a request in order.
pub fn apply_middleware(request: &mut Vec<String>, layers: &[String]) {
    for layer in layers {
        request.push(layer.clone());
    }
}

/// Rejects a request whose rate limit budget is already spent.
pub fn rate_limit(remaining: u32) -> bool {
    remaining > 0
}

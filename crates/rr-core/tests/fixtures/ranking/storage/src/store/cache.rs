//! Bounded cache with a least recently used replacement policy.

/// Cache of recently touched keys, oldest first.
pub struct Cache {
    /// Keys in touch order, oldest first.
    pub order: Vec<String>,
    /// Maximum number of keys the cache holds.
    pub capacity: usize,
}

/// Chooses and drops the least recently used key when the cache is full.
pub fn evict(cache: &mut Cache) -> Option<String> {
    if cache.order.len() <= cache.capacity {
        return None;
    }
    Some(cache.order.remove(0))
}

/// Records that a key was touched, moving it to the newest position.
pub fn touch(cache: &mut Cache, key: &str) {
    cache.order.retain(|held| held != key);
    cache.order.push(key.to_string());
}

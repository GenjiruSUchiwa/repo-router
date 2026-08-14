//! Retry policy for failed outbound requests.

/// Retries a failed request until the attempt budget is spent.
pub fn retry_with_backoff(attempts: u32) -> u64 {
    let mut waited = 0;
    for attempt in 0..attempts {
        waited += backoff_delay(attempt);
    }
    waited
}

/// Computes the exponential delay before the next attempt.
pub fn backoff_delay(attempt: u32) -> u64 {
    100u64 << attempt.min(10)
}

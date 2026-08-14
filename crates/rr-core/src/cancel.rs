//! Cooperative cancellation for long-running repository work.
//!
//! A token is a shared flag, not a channel: every stage of a refresh polls it
//! at its own boundaries, and the flag can be handed to `gix` — which wants an
//! `Arc<AtomicBool>` — without a second cancellation mechanism existing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared, clonable "stop as soon as it is safe to" flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Creates a token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Idempotent, and safe to call from a signal handler.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Returns the underlying flag, for handing to a library that owns its own
    /// interrupt handling.
    ///
    /// Sharing the flag rather than mirroring it is deliberate: two flags could
    /// disagree, and a stage that observed the stale one would keep working
    /// after the other had already given up.
    #[must_use]
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

#[cfg(test)]
mod tests {
    use super::CancelToken;

    #[test]
    fn a_clone_observes_the_original_cancellation() {
        let token = CancelToken::new();
        let clone = token.clone();

        assert!(!clone.is_cancelled());
        token.cancel();

        assert!(clone.is_cancelled());
        assert!(clone.flag().load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn cancelling_twice_stays_cancelled() {
        let token = CancelToken::new();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }
}

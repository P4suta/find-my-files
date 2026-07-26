//! Platform-independent cooperative cancellation for query execution.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cloneable cooperative-cancellation state for one query.
///
/// Every clone observes the same monotonic false→true transition, and
/// cancelling is idempotent. The Windows engine re-exports this type as part
/// of its public API.
#[derive(Clone, Debug, Default)]
pub struct QueryCancellation {
    cancelled: Arc<AtomicBool>,
}

impl QueryCancellation {
    /// Create a live query cancellation token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. The transition is monotonic and idempotent.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Whether two values refer to the same query lifecycle.
    #[must_use]
    pub fn is_same_query(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }

    pub(crate) fn check(&self) -> Result<(), QueryCancelled> {
        if self.is_cancelled() {
            Err(QueryCancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("query cancelled")]
pub struct QueryCancelled;

#[cfg(test)]
mod tests {
    use super::QueryCancellation;

    #[test]
    fn clones_share_one_monotonic_cancellation_lifecycle() {
        let token = QueryCancellation::new();
        let clone = token.clone();
        let other = QueryCancellation::new();

        assert!(token.is_same_query(&clone));
        assert!(!token.is_same_query(&other));
        assert!(token.check().is_ok());

        clone.cancel();
        clone.cancel();

        assert!(token.is_cancelled());
        assert!(clone.check().is_err());
        assert!(other.check().is_ok());
    }
}

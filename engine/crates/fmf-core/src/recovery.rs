//! Bounded recovery decisions for one persistent volume worker.

use core::time::Duration;

/// Remaining delay before the single automatic retry can begin.
#[must_use]
pub const fn backoff_remaining(elapsed: Duration) -> Duration {
    Duration::from_secs(2).saturating_sub(elapsed)
}

/// Failure classification used by the worker's panic boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// An ordinary acquisition, scan, or journal error.
    Error,
    /// A panic caught while running one attempt.
    Panic,
}

/// Recovery state between indexing attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryState {
    /// The active attempt can spend one automatic retry.
    AutomaticAvailable,
    /// The active attempt has already spent its automatic retry.
    AutomaticSpent,
    /// An ordinary failure is waiting for its bounded backoff.
    Backoff,
    /// Only an explicit request can start another attempt.
    AwaitRequest,
}

impl RetryState {
    /// Classify a failed attempt without rearming its retry budget.
    #[must_use]
    pub const fn failed(self, kind: FailureKind) -> Self {
        if matches!(self, Self::AutomaticAvailable) && matches!(kind, FailureKind::Error) {
            Self::Backoff
        } else {
            Self::AwaitRequest
        }
    }

    /// Consume a completed backoff without replenishing the budget.
    #[must_use]
    pub const fn elapsed(self) -> Self {
        match self {
            Self::Backoff => Self::AutomaticSpent,
            Self::AutomaticAvailable | Self::AutomaticSpent | Self::AwaitRequest => self,
        }
    }

    /// Rearm a failed worker once; active workers ignore duplicate requests.
    #[must_use]
    pub const fn requested(self) -> Self {
        match self {
            Self::Backoff | Self::AwaitRequest => Self::AutomaticAvailable,
            Self::AutomaticAvailable | Self::AutomaticSpent => self,
        }
    }
}

#[cfg(kani)]
#[path = "recovery_proofs.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::{FailureKind, RetryState, backoff_remaining};
    use core::time::Duration;

    #[test]
    fn backoff_releases_only_after_the_full_delay() {
        assert_eq!(backoff_remaining(Duration::ZERO), Duration::from_secs(2));
        assert_eq!(
            backoff_remaining(Duration::from_millis(1999)),
            Duration::from_millis(1)
        );
        assert_eq!(backoff_remaining(Duration::from_secs(2)), Duration::ZERO);
        assert_eq!(backoff_remaining(Duration::MAX), Duration::ZERO);
    }

    #[test]
    fn recovery_spends_one_automatic_retry_and_rearms_only_on_request() {
        let first = RetryState::AutomaticAvailable.failed(FailureKind::Error);
        assert_eq!(first, RetryState::Backoff);
        let automatic = first.elapsed();
        assert_eq!(automatic, RetryState::AutomaticSpent);
        let exhausted = automatic.failed(FailureKind::Error);
        assert_eq!(exhausted, RetryState::AwaitRequest);
        assert_eq!(exhausted.elapsed(), RetryState::AwaitRequest);
        assert_eq!(exhausted.requested(), RetryState::AutomaticAvailable);
    }

    #[test]
    fn active_requests_do_not_create_a_future_retry_allowance() {
        for state in [RetryState::AutomaticAvailable, RetryState::AutomaticSpent] {
            assert_eq!(state.requested(), state);
            assert_eq!(state.elapsed(), state);
        }
    }

    #[test]
    fn panics_always_wait_for_an_explicit_request() {
        for state in [
            RetryState::AutomaticAvailable,
            RetryState::AutomaticSpent,
            RetryState::Backoff,
            RetryState::AwaitRequest,
        ] {
            let failed = state.failed(FailureKind::Panic);
            assert_eq!(failed, RetryState::AwaitRequest);
            assert_eq!(failed.elapsed(), RetryState::AwaitRequest);
            assert_eq!(failed.requested(), RetryState::AutomaticAvailable);
        }
    }
}

use super::{FailureKind, RetryState, backoff_remaining};
use core::time::Duration;

fn any_state() -> RetryState {
    match kani::any::<u8>() {
        0 => RetryState::AutomaticAvailable,
        1 => RetryState::AutomaticSpent,
        2 => RetryState::Backoff,
        _ => RetryState::AwaitRequest,
    }
}

#[kani::proof]
#[kani::unwind(1)]
fn failures_never_rearm_and_panics_never_back_off() {
    let state = any_state();
    let failed = state.failed(FailureKind::Error);
    assert_eq!(
        matches!(failed, RetryState::Backoff),
        matches!(state, RetryState::AutomaticAvailable)
    );
    assert!(matches!(
        failed,
        RetryState::Backoff | RetryState::AwaitRequest
    ));
    assert_eq!(state.failed(FailureKind::Panic), RetryState::AwaitRequest);
    kani::cover!(matches!(state, RetryState::AutomaticAvailable));
    kani::cover!(matches!(state, RetryState::AutomaticSpent));
    kani::cover!(matches!(state, RetryState::Backoff));
    kani::cover!(matches!(state, RetryState::AwaitRequest));
}

#[kani::proof]
#[kani::unwind(1)]
fn backoff_consumes_the_only_automatic_allowance() {
    let available: bool = kani::any();
    let state = if available {
        RetryState::AutomaticAvailable
    } else {
        RetryState::AutomaticSpent
    };
    let failed = state.failed(FailureKind::Error);
    let retried = failed.elapsed();
    assert_eq!(
        retried,
        if available {
            RetryState::AutomaticSpent
        } else {
            RetryState::AwaitRequest
        }
    );
    assert_eq!(retried.elapsed(), retried);
    assert_eq!(retried.failed(FailureKind::Error), RetryState::AwaitRequest);
    kani::cover!(available);
    kani::cover!(!available);
}

#[kani::proof]
#[kani::unwind(1)]
fn explicit_requests_rearm_only_failed_workers() {
    let state = any_state();
    let next = state.requested();
    if matches!(state, RetryState::Backoff | RetryState::AwaitRequest) {
        assert_eq!(next, RetryState::AutomaticAvailable);
    } else {
        assert_eq!(next, state);
    }
    assert_eq!(next.requested(), next);
    kani::cover!(matches!(state, RetryState::AutomaticSpent));
    kani::cover!(matches!(state, RetryState::Backoff));
    kani::cover!(matches!(state, RetryState::AwaitRequest));
}

#[kani::proof]
#[kani::unwind(1)]
fn automatic_backoff_is_exact_for_every_elapsed_duration() {
    let seconds: u64 = kani::any();
    let nanos: u32 = kani::any();
    kani::assume(nanos < 1_000_000_000);
    let elapsed = Duration::new(seconds, nanos);
    let remaining = backoff_remaining(elapsed);
    assert!(remaining <= Duration::from_secs(2));
    assert_eq!(remaining.is_zero(), elapsed >= Duration::from_secs(2));
    if elapsed < Duration::from_secs(2) {
        assert_eq!(remaining + elapsed, Duration::from_secs(2));
    }
    kani::cover!(elapsed.is_zero());
    kani::cover!(elapsed == Duration::from_secs(2));
    kani::cover!(elapsed > Duration::from_secs(2));
}

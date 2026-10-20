use super::Summary;

#[kani::proof]
#[kani::unwind(2)]
fn accepted_summary_requires_every_registered_harness_to_pass() {
    let summary = Summary {
        total_harnesses: kani::any(),
        executed: kani::any(),
        successful: kani::any(),
        failed: kani::any(),
    };
    let expected: usize = kani::any();
    let complete = summary.complete(expected);
    if complete {
        assert!(expected > 0);
        assert_eq!(summary.total_harnesses, expected);
        assert_eq!(summary.executed, expected);
        assert_eq!(summary.successful, expected);
        assert_eq!(summary.failed, 0);
    }
    if expected == 0 {
        assert!(!complete);
    }
    kani::cover!(complete);
    kani::cover!(!complete);
}

#[kani::proof]
#[kani::unwind(2)]
fn a_complete_nonempty_summary_remains_acceptable() {
    let expected: usize = kani::any();
    kani::assume(expected > 0);
    let summary = Summary {
        total_harnesses: expected,
        executed: expected,
        successful: expected,
        failed: 0,
    };
    assert!(summary.complete(expected));
    kani::cover!(expected == 1);
    kani::cover!(expected == usize::MAX);
}

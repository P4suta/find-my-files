use super::concurrency::{Plan, TEST_THREADS_PER_JOB};
use core::num::NonZeroUsize;

#[kani::proof]
#[kani::unwind(2)]
fn activation_requires_executed_kills_and_no_unresolved_outcomes() {
    let counts = super::smoke::ActivationCounts {
        killed: kani::any(),
        survived: kani::any(),
        no_coverage: kani::any(),
        timeouts: kani::any(),
        ignored: kani::any(),
        outside_scope: kani::any(),
    };
    if counts.verified() {
        assert!(counts.killed > 0);
        assert_eq!(counts.survived, 0);
        assert_eq!(counts.no_coverage, 0);
        assert_eq!(counts.timeouts, 0);
        assert_eq!(counts.ignored, 0);
        assert_eq!(counts.outside_scope, 0);
    } else {
        assert!(
            counts.killed == 0
                || counts.survived > 0
                || counts.no_coverage > 0
                || counts.timeouts > 0
                || counts.ignored > 0
                || counts.outside_scope > 0
        );
    }
    kani::cover!(counts.verified() && counts.killed == usize::MAX);
    kani::cover!(!counts.verified());
}

#[kani::proof]
#[kani::unwind(2)]
fn test_slots_fit_every_nonzero_cpu_budget_without_overflow() {
    let cpus: usize = kani::any();
    kani::assume(cpus > 0);
    let plan = Plan::for_cpus(NonZeroUsize::new(cpus).expect("nonzero CPU budget"));
    assert!(plan.jobs() > 0);
    assert!(plan.test_threads() > 0);
    assert!(plan.test_threads() <= TEST_THREADS_PER_JOB);
    let slots = plan.jobs() * plan.test_threads();
    assert!(slots <= cpus);
    assert!(cpus - slots < plan.test_threads());
    if cpus < TEST_THREADS_PER_JOB {
        assert_eq!(plan.jobs(), 1);
        assert_eq!(plan.test_threads(), cpus);
    } else {
        assert_eq!(plan.test_threads(), TEST_THREADS_PER_JOB);
    }
    kani::cover!(cpus == 1);
    kani::cover!(cpus == TEST_THREADS_PER_JOB - 1);
    kani::cover!(cpus == TEST_THREADS_PER_JOB);
    kani::cover!(cpus == usize::MAX);
}

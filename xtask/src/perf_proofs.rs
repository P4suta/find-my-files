use super::{counter_value_is_valid, ExecutionConfig};

#[kani::proof]
#[kani::unwind(1)]
fn admitted_counter_values_are_finite_and_nonnegative() {
    let value = f64::from_bits(kani::any());
    let accepted = counter_value_is_valid(value);
    if accepted {
        assert!(value.is_finite());
        assert!(value >= 0.0);
    } else {
        assert!(!value.is_finite() || value < 0.0);
    }
    kani::cover!(accepted && value > 100.0);
    kani::cover!(!accepted && value == -1.0);
    kani::cover!(!accepted && value.is_nan());
}

#[kani::proof]
#[kani::unwind(1)]
fn execution_plans_use_only_assigned_cpus_and_positive_bounded_workers() {
    let system: usize = kani::any();
    let assigned: usize = kani::any();
    let requested: Option<usize> = kani::any();
    let result = ExecutionConfig::plan(system, assigned, requested);
    let valid_assignment = assigned != 0 && system != 0 && assigned & !system == 0;
    let capacity = assigned.count_ones() as usize;
    let workers = requested.unwrap_or(capacity);
    let valid_workers = workers > 0 && workers <= capacity;
    assert_eq!(result.is_ok(), valid_assignment && valid_workers);
    if let Ok(plan) = result {
        assert_eq!(plan.system_mask.get(), system);
        assert_eq!(plan.cpu_mask.get(), assigned);
        assert_eq!(plan.rayon_workers.get(), workers);
        assert!(plan.rayon_workers.get() <= plan.cpu_mask.get().count_ones() as usize);
    }
    kani::cover!(result.is_ok() && requested.is_none());
    kani::cover!(result.is_ok() && requested.is_some());
    kani::cover!(result.is_err() && valid_assignment);
    kani::cover!(result.is_err() && !valid_assignment);
}

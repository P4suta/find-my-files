use super::ExecutionConfig;

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

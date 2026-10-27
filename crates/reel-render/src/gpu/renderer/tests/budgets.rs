use super::*;
#[test]
fn local_demand_and_shared_pressure_release_independently() {
    let limits = crate::gpu::GpuLimits {
        target_bytes: 10,
        ..Default::default()
    };
    let shared = Budgets::new(limits);
    let first = Budgets::new(limits);
    let second = Budgets::new(limits);
    let old = reserve(&shared.targets, &first.targets, 8, 10, "targets").unwrap();
    assert!(matches!(
        reserve(&shared.targets, &second.targets, 8, 10, "targets"),
        Err(GpuError::Pressure { available: 2, .. })
    ));
    assert_eq!(second.targets.usage().bytes, 0);
    assert!(matches!(
        reserve(&shared.targets, &second.targets, 11, 10, "targets"),
        Err(GpuError::Budget { available: 10, .. })
    ));
    drop(old);
    let target = reserve(&shared.targets, &second.targets, 8, 10, "targets").unwrap();
    assert!(matches!(
        reserve(&shared.targets, &second.targets, 3, 10, "targets"),
        Err(GpuError::Budget { available: 2, .. })
    ));
    drop(target);
    assert_eq!(shared.targets.usage().bytes, 0);
    assert_eq!(second.targets.usage().bytes, 0);
}

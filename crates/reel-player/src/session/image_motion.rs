//! Repeated sprite transforms keep unchanged viewport resource demand settled.
use super::*;

#[test]
fn layered_motion_reuses_resolved_viewport_images() {
    let source = r#"fn main() {
        let world = stack(width: 640, height: 420, clip: true);
        for i in 0..24 {
            let picture = image(if i % 2 == 0 { "assets/sun.jpg" } else { "assets/panels.png" },
                x: (i % 8) * 55, y: (i / 8) * 85, width: 96, height: 96);
            world.add(picture);
            run(tween(picture, x: (i % 8) * 55 + 80, rotation: 12, opacity: 0.65, duration: 100));
        }
        stage.replace(world);
        narrator("moving");
    }"#;
    let (mut driver, mut bridge, root, wake) =
        fixture_with_source(source, crate::assets::tests::memory_assets());
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let _ = frame(&mut driver, &mut bridge, root, 1);
    let loaded = driver.ui().images.loads;
    let resources = driver.ui().stage.as_ref().unwrap().resources.clone();
    let mut reused = 0;
    let start = std::time::Instant::now();
    let allocations = allocation_counter::measure(|| {
        for tick in 1..=240 {
            while driver
                .advance_time(f64::from(tick) / 60., 4096)
                .unwrap()
                .pending
            {}
            driver.sync(&mut bridge, root).unwrap();
            reused += usize::from(Arc::ptr_eq(
                &resources,
                &driver.ui().stage.as_ref().unwrap().resources,
            ));
        }
    });
    println!(
        "layered_motion sprites=24 updates=240 sync_us={} allocations={} bytes={} resource_reuses={reused}",
        start.elapsed().as_micros(),
        allocations.count_total,
        allocations.bytes_total
    );
    assert_eq!(driver.ui().images.loads, loaded);
    assert!(!image_pixel_ids(&frame(&mut driver, &mut bridge, root, 2)).is_empty());
    assert_eq!(
        reused, 240,
        "unchanged required resources must retain resolved leases"
    );
}

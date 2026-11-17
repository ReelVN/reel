//! Capture owns temporary targets while the displayed frame keeps its caches.
use super::*;
use reel_core::wake::WakeSignal;

#[test]
#[ignore = "requires native WGPU and REEL_R01_TEST_FONT"]
fn thumbnail_keeps_live_targets_and_geometry_during_and_after_readback() {
    let (_pool, _registry, _font, frame, _bridge) = long_text_fixture();
    let mut subject = renderer(Default::default());
    let output = pool();
    let live = subject.prepare(&frame).unwrap();
    let live_texture = live.texture.clone();
    subject.submit(live).unwrap();
    subject
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    subject.poll().unwrap();
    for _ in 0..3 {
        subject.submit(subject.prepare(&frame).unwrap()).unwrap();
        while subject.has_pending() {
            subject
                .gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            subject.poll().unwrap();
        }
    }
    let targets = subject.usage().targets;
    let mut thumbnail = subject
        .capture(&frame, [200, 150], &output, WakeSignal::new(|| {}))
        .unwrap();
    let geometry = subject.geometry.uploaded_bytes.load(Ordering::Relaxed);
    let next = subject.prepare(&frame).unwrap();
    assert_eq!(
        next.texture, live_texture,
        "capture must retain the viewport pool"
    );
    assert_eq!(
        subject.geometry.uploaded_bytes.load(Ordering::Relaxed),
        geometry,
        "capture must preserve the live frame's prepared geometry"
    );
    subject.submit(next).unwrap();
    subject
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let pixels = subject
        .poll_capture(&mut thumbnail, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(pixels.rgba.as_slice().len(), 200 * 150 * 4);
    drop((pixels, thumbnail));
    subject.poll().unwrap();
    assert_eq!(
        subject.usage().targets,
        targets,
        "temporary capture targets retire"
    );
    assert_eq!(output.usage().bytes, 0);
}

#[test]
#[ignore = "requires native WGPU; exercises actual shared target pressure"]
fn capture_reclaims_idle_live_targets_only_when_its_budget_requires_it() {
    let pool = pool();
    let frame = frame(workload(1, 0, Color::WHITE), &pool);
    let mut subject = renderer(crate::gpu::GpuLimits {
        target_bytes: 64 * 48 * 8 * 5,
        ..Default::default()
    });
    subject.submit(subject.prepare(&frame).unwrap()).unwrap();
    subject
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    subject.poll().unwrap();
    assert_eq!(subject.usage().targets, subject.gpu.limits.target_bytes);
    let mut capture = subject
        .capture(&frame, [32, 24], &pool, WakeSignal::new(|| {}))
        .unwrap();
    subject
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let pixels = subject
        .poll_capture(&mut capture, u64::MAX)
        .unwrap()
        .unwrap();
    assert!(
        pixels
            .rgba
            .as_slice()
            .chunks_exact(4)
            .all(|p| p == [255; 4])
    );
    drop((capture, pixels));
    subject.poll().unwrap();
    assert!(subject.prepare(&frame).is_ok());
}

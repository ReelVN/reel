//! Existing frame leases keep bounded uploads and warm textures reusable after
//! registry retirement. These checks submit and wait without reading pixels.
#![cfg(feature = "wgpu")]
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::{AssetKey, ContentVersion, FrameId, ResourceVersion};
use reel_ui::{
    frame::RenderFrame,
    resources::{ResourceRegistry, ResourceWeak},
};
static SUBMISSIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn font_frame() -> (
    RenderFrame,
    ResourceRegistry,
    reel_ui::resources::ResourceHandle,
    ResourceWeak,
) {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::{DefaultProperties, NewWidget, StyleProperty},
        dpi::PhysicalSize,
        peniko::Blob,
        resources::ResourceData,
        widgets::Label,
    };
    let pool = pool();
    let bytes =
        std::fs::read(std::env::var_os("REEL_R01_TEST_FONT").expect("REEL_R01_TEST_FONT")).unwrap();
    let ticket = pool.reserve(bytes.len() as u64).unwrap();
    let font = Blob::from(bytes);
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register(
            ResourceVersion {
                key: AssetKey("leased-font".into()),
                version: ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            ticket,
        )
        .unwrap();
    let weak = registry.acquire(handle).unwrap().downgrade();
    let root = RenderRoot::new(
        NewWidget::new(Label::new("Lease 012345 ABC abc").with_style(StyleProperty::FontSize(24.))),
        |_| {},
        RenderRootOptions {
            default_properties: std::sync::Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let mut bridge = r01_fixture::bridge(root);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    (frame, registry, handle, weak)
}
fn close_font(
    registry: &mut ResourceRegistry,
    handle: reel_ui::resources::ResourceHandle,
    retired: bool,
) {
    assert!(if retired {
        registry.retire(handle)
    } else {
        registry.release(handle)
    });
    assert!(registry.acquire(handle).is_err());
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires native WGPU and REEL_R01_TEST_FONT; no pixel readback"]
fn wgpu_glyph_cache_borrows_font_until_final_completed_frame() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let _serial = SUBMISSIONS.lock().unwrap();
    for retired in [false, true] {
        let (frame, mut registry, handle, weak) = font_frame();
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits::default(),
        ))
        .unwrap();
        let mut renderer = Renderer::new(gpu);
        renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(
            renderer.usage().resources,
            512 * 512,
            "single-channel atlas page"
        );
        close_font(&mut registry, handle, retired);
        #[cfg(feature = "profiling")]
        let before = reel_core::profile::Sample::snapshot();
        renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
        #[cfg(feature = "profiling")]
        assert_eq!(
            reel_core::profile::Sample::snapshot().delta(before).calls
                [reel_core::profile::Phase::GlyphRaster as usize],
            0
        );
        drop(frame);
        assert!(
            !weak.is_released(),
            "in-flight frame owns the final font lease"
        );
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert!(weak.is_released(), "the cache must not own font payloads");
        assert_eq!(renderer.usage().resources, 0);
    }
}

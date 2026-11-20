//! Synthetic page/view ownership and sampling; no game or window input.
use super::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, ResourceVersion},
    texture::{
        ColorEncoding, ImageView, TextureDescriptor, TextureFormat, TextureLayout, TexturePayload,
    },
};
use reel_ui::resources::ResourceRegistry;

fn fixture() -> (
    RenderFrame,
    ResourceRegistry,
    Vec<reel_ui::resources::ResourceHandle>,
) {
    let pool = pool();
    let mut registry = ResourceRegistry::new(3, 1 << 20).unwrap();
    let version = |name: &str| ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([1; 32]),
    };
    let descriptor = TextureDescriptor {
        storage_layout: Default::default(),
        mip_count: 1,
        format: TextureFormat::Rgba8,
        layout: TextureLayout::identity([8, 4]),
        source_encoding: ColorEncoding::Linear,
    };
    let mut bytes = Vec::new();
    for _ in 0..4 {
        for x in 0..8 {
            bytes.extend(if x < 4 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
        }
    }
    let count = bytes.len() as u64;
    let page = registry
        .register_texture(
            version("page"),
            TexturePayload::new(
                descriptor,
                OwnedBytes::new(bytes, pool.reserve(count).unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let page = registry.acquire(page).unwrap();
    let mut handles = Vec::new();
    let mut widget = RecordWidget::new(Scene::new());
    for (i, origin) in [0, 4].into_iter().enumerate() {
        let view = ImageView {
            logical_size: [4, 4],
            content_origin: [origin, 0],
            content_size: [4, 4],
        };
        let handle = registry
            .register_atlas_view(
                version(if i == 0 { "red" } else { "blue" }),
                view,
                ColorEncoding::Linear,
                page.clone(),
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        widget.resource_draws.push((
            lease,
            Rect::new(i as f64 * 200., 0., (i + 1) as f64 * 200., 300.),
            Affine::IDENTITY,
            Default::default(),
        ));
        handles.push(handle);
    }
    let snapshot = registry.freeze(&handles, &pool).unwrap();
    (
        frame_from_widget(widget, snapshot, &pool),
        registry,
        handles,
    )
}
#[test]
#[ignore = "requires graphics adapter; synthetic texture values only"]
fn shared_page_uploads_once_and_views_keep_color() {
    let (frame, _registry, _handles) = fixture();
    let plan = crate::gpu::image_plan::collect(
        [&frame].into_iter(),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(plan.images.len(), 1);
    assert_eq!(plan.aliases.len(), 2);
    let mut renderer = renderer(Default::default());
    let prepared = loop {
        match renderer.prepare(&frame) {
            Ok(prepared) => break prepared,
            Err(GpuError::Pressure { .. } | GpuError::Busy) => {
                renderer
                    .gpu
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                renderer.poll().unwrap();
            }
            Err(error) => panic!("atlas prepare: {error}"),
        }
    };
    assert_eq!(renderer.gpu.upload_creations.snapshot()[0], 1);
    let pixels = renderer.readback(prepared).unwrap();
    let red = pixels[24 * 64 + 16];
    let blue = pixels[24 * 64 + 48];
    assert!(red[0] > 0.9 && red[2] < 0.1, "{red:?}");
    assert!(blue[2] > 0.9 && blue[0] < 0.1, "{blue:?}");
}

#[test]
#[ignore = "requires graphics adapter; synthetic texture values only"]
fn shared_page_reloads_once_after_gpu_eviction() {
    let (frame, registry, handles) = fixture();
    let page = registry
        .acquire(handles[0])
        .unwrap()
        .image_backing()
        .clone();
    page.permit_pixel_reload(None);
    let mut renderer = renderer(Default::default());
    let prepared = loop {
        match renderer.prepare(&frame) {
            Ok(prepared) => break prepared,
            Err(GpuError::Pressure { .. } | GpuError::Busy) => {
                renderer
                    .gpu
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                renderer.poll().unwrap();
            }
            Err(error) => panic!("atlas prepare: {error}"),
        }
    };
    renderer.submit(prepared).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert!(!page.pixels_resident());
    renderer.images.retain_demand(&[]);
    let demand = match renderer.prepare(&frame) {
        Err(GpuError::PixelsNeeded(demand)) => demand,
        _ => panic!("evicted page must request its pixels"),
    };
    assert_eq!(demand.resources().len(), 1);
    assert_eq!(demand.resources()[0].handle(), page.handle());
    let (_, replacement, other_handles) = fixture();
    let other = replacement
        .acquire(other_handles[0])
        .unwrap()
        .image_backing()
        .clone();
    page.restore_pixels(&other).unwrap();
    assert!(renderer.prepare(&frame).is_ok());
}

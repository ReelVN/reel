use super::*;
use reel_core::{
    budget::{BudgetTicket, OwnedBytes},
    identity::{AssetKey, ContentVersion, ResourceVersion},
    image::{Yuv420Image, Yuv420Storage, YuvColor, YuvMatrix, YuvTransfer},
};
use reel_ui::{
    frame::{
        BuildLimits, FrameBuild, FrameInfo, RetainBuild, RetainedScene, SavedRetainedScene,
        StepBudget,
    },
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry, ResourceSnapshot},
    stage::ImageSource,
};

const STEP: StepBudget = StepBudget {
    max_commands: 1,
    max_units: 8192,
    max_bytes: 64 << 10,
};
const LIMITS: BuildLimits = BuildLimits { max_bytes: 8 << 20 };

#[derive(Debug)]
struct Planes {
    planes: [Vec<u8>; 3],
    _ticket: BudgetTicket,
}
impl Yuv420Storage for Planes {
    fn plane(&self, index: usize) -> &[u8] {
        &self.planes[index]
    }
}
fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 256,
            max_bytes: 32 << 20,
        },
    )
}
fn kind(lease: ResourceLease) -> NodeKind {
    NodeKind::Image {
        source: ImageSource::resource(lease),
        origin: None,
        fit: Default::default(),
        slice: None,
        slice_scale: 1.,
        slice_center: true,
        repeat: [false; 2],
        tile_scale: 1.,
        corner_colors: None,
    }
}
fn picture(first: ResourceLease, second: ResourceLease, pool: &BudgetPool) -> RetainedScene {
    let mut fixture = Fixture::new(None);
    for (id, lease) in [(fixture.red, first), (fixture.blue, second)] {
        let props = fixture.stage.node(id).unwrap().props.clone();
        fixture.stage.update(id, kind(lease), props).unwrap();
    }
    let recorded = fixture.record();
    fixture.accept(&recorded);
    let resources =
        ResourceSnapshot::capture(fixture.ui.image_resources().iter().cloned().map(Ok), pool)
            .unwrap();
    let mut frame = FrameBuild::new(
        recorded.source,
        resources,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        LIMITS,
        pool,
    )
    .unwrap();
    while !frame.step(STEP).unwrap().complete {}
    let frame = frame.finish().unwrap();
    let mut retain = RetainBuild::new(&frame, None, LIMITS, pool).unwrap();
    while !retain.step(STEP).unwrap().complete {}
    retain.finish().unwrap()
}
fn restore_inline(saved: &SavedRetainedScene, pool: &BudgetPool) -> RetainedScene {
    let mut restore = saved.restore_begin(LIMITS, pool).unwrap();
    loop {
        let progress = restore.step(STEP).unwrap();
        assert_eq!(progress.missing_asset, None);
        if progress.build.complete {
            break;
        }
    }
    restore.finish().unwrap()
}
fn version(name: &str, byte: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(name.into()),
        version: ContentVersion([byte; 32]),
    }
}

#[test]
fn retained_inline_rgba_and_yuv_share_pixels_then_restore_color_and_planes() {
    for (matrix, full_range, transfer) in [
        (YuvMatrix::Bt601, true, YuvTransfer::Srgb),
        (YuvMatrix::Bt709, false, YuvTransfer::Bt709),
    ] {
        let storage_pool = pool();
        let frame_pool = pool();
        let planes = Arc::new(Planes {
            planes: [
                vec![16, 32, 48, 64, 80, 96, 112, 128, 235],
                vec![90, 110, 130, 150],
                vec![70, 100, 140, 180],
            ],
            _ticket: storage_pool.reserve(17).unwrap(),
        });
        let image = Yuv420Image::new(
            [3, 3],
            YuvColor {
                matrix,
                full_range,
                transfer,
            },
            planes,
        )
        .unwrap();
        let mut registry = ResourceRegistry::new(2, 64 << 10).unwrap();
        let yuv = registry
            .register_yuv420(version("dynamic-video", 1), image.clone())
            .unwrap();
        let rgba_bytes = [64, 128, 192, 255].repeat(64 * 64);
        let rgba = registry
            .register_image(
                version("dynamic-image", 2),
                OwnedBytes::new(
                    rgba_bytes.clone(),
                    storage_pool.reserve(rgba_bytes.len() as u64).unwrap(),
                )
                .unwrap(),
                [64, 64],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Linear,
            )
            .unwrap();
        let rgba_lease = registry.acquire(rgba).unwrap();
        let ResourceData::Image {
            image: original_rgba,
            ..
        } = rgba_lease.data()
        else {
            panic!()
        };
        let rgba_pointer = original_rgba.data.as_ref().as_ptr();
        let scene = picture(
            registry.acquire(yuv).unwrap(),
            rgba_lease.clone(),
            &frame_pool,
        );
        let saved = scene.snapshot(|_| None).unwrap();
        assert_eq!(saved.assets().count(), 0);
        let live = restore_inline(&saved, &frame_pool);
        for lease in live.drawing().resources().leases() {
            match lease.data() {
                ResourceData::Yuv420(retained) => assert!(
                    retained.same_storage(&image),
                    "making a save descriptor copied decoded planes"
                ),
                ResourceData::Image { image, encoding } => {
                    assert_eq!(
                        image.data.as_ref().as_ptr(),
                        rgba_pointer,
                        "making a save descriptor copied the dynamic image"
                    );
                    assert_eq!(*encoding, ColorEncoding::Linear);
                }
                _ => panic!("unexpected picture resource"),
            }
        }
        let bytes = postcard::to_stdvec(&saved).unwrap();
        let decoded: SavedRetainedScene = postcard::from_bytes(&bytes).unwrap();
        let restored = restore_inline(&decoded, &frame_pool);
        assert_eq!(restored.drawing().resources().leases().len(), 2);
        for lease in restored.drawing().resources().leases() {
            match lease.data() {
                ResourceData::Yuv420(restored) => {
                    assert_eq!(restored.size(), [3, 3]);
                    assert_eq!(restored.color(), image.color());
                    for plane in 0..3 {
                        assert_eq!(restored.plane_size(plane), image.plane_size(plane));
                        assert_eq!(restored.plane(plane), image.plane(plane));
                    }
                }
                ResourceData::Image { image, encoding } => {
                    assert_eq!([image.width, image.height], [64, 64]);
                    assert_eq!(image.format, ImageFormat::Rgba8);
                    assert_eq!(image.alpha_type, ImageAlphaType::Alpha);
                    assert_eq!(*encoding, ColorEncoding::Linear);
                    assert_eq!(image.data.as_ref(), rgba_bytes);
                }
                _ => panic!("unexpected restored resource"),
            }
        }
        drop((
            restored, decoded, live, saved, scene, registry, rgba_lease, image,
        ));
        assert_eq!(
            storage_pool.usage().bytes,
            0,
            "producer pixels outlived their final drawing"
        );
        assert_eq!(
            frame_pool.usage().bytes,
            0,
            "retained drawing or restore budget was not returned"
        );
    }
}

#[test]
fn retained_file_images_save_version_references_and_reject_wrong_rebinding() {
    let assets = crate::assets::tests::memory_assets();
    let load = |path: &str| {
        let id = assets.request(path, crate::assets::AssetKind::Image);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(lease) = assets.poll(id).unwrap() {
                break lease;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    let first = load("assets/panels.png");
    let second = load("assets/sun.jpg");
    let pool = pool();
    let scene = picture(first.clone(), second.clone(), &pool);
    let saved = scene
        .snapshot(|lease| assets.retained_asset(lease))
        .unwrap();
    assert_eq!(saved.assets().count(), 2);
    let bytes = postcard::to_stdvec(&saved).unwrap();
    assert!(
        bytes.len() < 8 << 10,
        "ordinary image pixels entered the save: {} bytes",
        bytes.len()
    );
    let saved: SavedRetainedScene = postcard::from_bytes(&bytes).unwrap();
    let mut restore = saved.restore_begin(LIMITS, &pool).unwrap();
    let (slot, asset) = saved.assets().next().unwrap();
    let wrong = if asset.version == *first.version() {
        &second
    } else {
        &first
    };
    assert!(restore.bind_asset(slot, wrong.clone()).is_err());
    for (slot, asset) in saved.assets() {
        let lease = if asset.version == *first.version() {
            &first
        } else {
            &second
        };
        restore.bind_asset(slot, lease.clone()).unwrap();
    }
    while !restore.step(STEP).unwrap().build.complete {}
    let restored = restore.finish().unwrap();
    assert_eq!(restored.drawing().resources().leases().len(), 2);
}

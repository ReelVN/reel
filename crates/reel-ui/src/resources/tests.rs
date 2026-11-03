use super::*;
use reel_core::identity::{AssetKey, ContentVersion};

fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 10000,
            max_bytes: 64 << 20,
        },
    )
}
fn version(i: usize) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(format!("resource/{i}")),
        version: ContentVersion([i as u8; 32]),
    }
}
fn external(registry: &mut ResourceRegistry, p: &BudgetPool, i: usize) -> ResourceLease {
    let h = registry
        .register(
            version(i),
            ResourceData::External { token: i as u64 },
            p.reserve(0).unwrap(),
        )
        .unwrap();
    registry.acquire(h).unwrap()
}

#[test]
fn indexed_slots_preserve_versions_until_last_lease_and_recover_after_rejection() {
    let p = pool();
    let mut registry = ResourceRegistry::new(1, 1 << 20).unwrap();
    let first = external(&mut registry, &p, 1);
    let register = |registry: &mut ResourceRegistry, i| {
        registry.register(
            version(i),
            ResourceData::External { token: i as u64 },
            p.reserve(0).unwrap(),
        )
    };
    assert_eq!(register(&mut registry, 1), Ok(first.handle()));
    assert!(registry.release(first.handle()));
    assert!(!registry.release(first.handle()));
    assert_eq!(register(&mut registry, 1), Ok(first.handle()));
    assert_eq!(register(&mut registry, 2), Err(ResourceError::Capacity));
    let old = first.handle();
    drop(first);
    let second = external(&mut registry, &p, 2);
    assert_eq!(second.handle().slot, old.slot);
    assert_ne!(second.handle().generation, old.generation);
    assert!(registry.acquire(old).is_err());
    assert!(registry.retire(second.handle()));
    assert_eq!(
        register(&mut registry, 2),
        Err(ResourceError::RetiredOrForeign)
    );
    assert_eq!(register(&mut registry, 3), Err(ResourceError::Capacity));
    drop(second);
    assert!(register(&mut registry, 3).is_ok());
}
fn capture(values: &[ResourceLease], p: &BudgetPool) -> Result<ResourceSnapshot, ResourceError> {
    ResourceSnapshot::capture(values.iter().cloned().map(Ok), p)
}
fn linear(values: &[ResourceLease], p: &BudgetPool) -> Result<ResourceSnapshot, ResourceError> {
    let ticket = p
        .reserve(std::mem::size_of_val(values) as u64)
        .map_err(|_| ResourceError::Bytes)?;
    let mut leases = Vec::with_capacity(values.len());
    for lease in values.iter().cloned() {
        if lease.is_retired() {
            return Err(ResourceError::RetiredOrForeign);
        }
        if leases.iter().any(|other: &ResourceLease| {
            other.handle() == lease.handle() || other.lookup_same_source(&lease)
        }) {
            return Err(ResourceError::ConflictingVersion);
        }
        leases.push(lease);
    }
    leases.sort_unstable_by_key(|r| {
        (
            r.source_blob_id().is_none(),
            r.source_blob_id().unwrap_or(0),
            r.handle(),
        )
    });
    Ok(ResourceSnapshot {
        leases,
        _ticket: ticket,
    })
}

#[test]
fn indexed_capture_preserves_conflicts_order_lifetimes_and_budget() {
    let p = pool();
    let mut registry = ResourceRegistry::new(80, 1 << 20).unwrap();
    let leases: Vec<_> = (0..40)
        .rev()
        .map(|i| external(&mut registry, &p, i))
        .collect();
    for count in [0, 1, 15, 16, 40] {
        let before = p.usage().bytes;
        let a = capture(&leases[..count], &p).unwrap();
        assert_eq!(
            p.usage().bytes - before,
            (count * size_of::<ResourceLease>()) as u64
        );
        let b = linear(&leases[..count], &p).unwrap();
        assert_eq!(
            a.leases()
                .iter()
                .map(ResourceLease::handle)
                .collect::<Vec<_>>(),
            b.leases()
                .iter()
                .map(ResourceLease::handle)
                .collect::<Vec<_>>()
        );
        drop((a, b));
        assert_eq!(p.usage().bytes, before);
    }
    let mut dup = leases.clone();
    dup.push(leases[3].clone());
    assert_eq!(
        capture(&dup, &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
    // Earlier conflict must win over an iterator error or a later retired lease.
    let mut results: Vec<_> = dup.iter().cloned().map(Ok).collect();
    results.push(Err(ResourceError::Bytes));
    assert_eq!(
        ResourceSnapshot::capture(results.into_iter(), &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
    let tight = BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 1,
            max_bytes: (leases.len() * size_of::<ResourceLease>() - 1) as u64,
        },
    );
    assert_eq!(capture(&leases, &tight).unwrap_err(), ResourceError::Bytes);
    assert_eq!(tight.usage().bytes, 0);
    let snapshot = capture(&leases, &p).unwrap();
    registry.retire(leases[0].handle());
    assert!(
        snapshot
            .leases()
            .iter()
            .any(|v| v.handle() == leases[0].handle())
    );
    assert_eq!(
        capture(&leases, &p).unwrap_err(),
        ResourceError::RetiredOrForeign
    );
    drop(snapshot);
}

#[test]
fn indexed_capture_distinguishes_source_aliases_and_blob_id_collisions() {
    let p = pool();
    let mut a = ResourceRegistry::new(40, 1 << 20).unwrap();
    let mut b = ResourceRegistry::new(4, 1 << 20).unwrap();
    let mut values: Vec<_> = (0..20).map(|i| external(&mut a, &p, i)).collect();
    let one: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new([1u8, 2, 3, 4]);
    let two: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new([5u8, 6, 7, 8]);
    let blob_a = Blob::from_raw_parts(one, 7);
    let blob_b = Blob::from_raw_parts(two, 7);
    let h1 = a
        .register(
            version(100),
            ResourceData::Font(blob_a.clone()),
            p.reserve(4).unwrap(),
        )
        .unwrap();
    let h2 = b
        .register(
            version(101),
            ResourceData::Font(blob_b),
            p.reserve(4).unwrap(),
        )
        .unwrap();
    let h3 = b
        .register(
            version(102),
            ResourceData::Font(blob_a),
            p.reserve(4).unwrap(),
        )
        .unwrap();
    values.push(a.acquire(h1).unwrap());
    values.push(b.acquire(h2).unwrap());
    assert!(
        capture(&values, &p).is_ok(),
        "same blob ID does not imply equal source"
    );
    values.push(b.acquire(h3).unwrap());
    assert_eq!(
        capture(&values, &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
}

#[derive(Debug)]
struct Planes([Vec<u8>; 3]);
impl reel_core::image::Yuv420Storage for Planes {
    fn plane(&self, index: usize) -> &[u8] {
        &self.0[index]
    }
}
#[test]
fn indexed_capture_matches_image_encoding_and_yuv_storage_rules() {
    use reel_core::image::{Yuv420Image, YuvColor, YuvMatrix, YuvTransfer};
    let p = pool();
    let mut a = ResourceRegistry::new(32, 1 << 20).unwrap();
    let mut b = ResourceRegistry::new(8, 1 << 20).unwrap();
    let prefix: Vec<_> = (0..20).map(|i| external(&mut a, &p, i)).collect();
    let image = ImageData {
        data: Blob::new(Arc::new([255u8; 4])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 1,
        height: 1,
    };
    let h1 = a
        .register(
            version(100),
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb,
            },
            p.reserve(4).unwrap(),
        )
        .unwrap();
    let h2 = b
        .register(
            version(101),
            ResourceData::Image {
                image,
                encoding: ColorEncoding::Linear,
            },
            p.reserve(4).unwrap(),
        )
        .unwrap();
    let mut values = prefix.clone();
    values.push(a.acquire(h1).unwrap());
    values.push(b.acquire(h2).unwrap());
    assert_eq!(
        capture(&values, &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
    let color = YuvColor {
        matrix: YuvMatrix::Bt601,
        full_range: false,
        transfer: YuvTransfer::Bt709,
    };
    let make = || {
        Yuv420Image::new(
            [2, 2],
            color,
            Arc::new(Planes([vec![0; 4], vec![0; 1], vec![0; 1]])),
        )
        .unwrap()
    };
    let one = make();
    let other = make();
    let y1 = a.register_yuv420(version(102), one.clone()).unwrap();
    let y2 = b.register_yuv420(version(103), other).unwrap();
    let y3 = b.register_yuv420(version(104), one).unwrap();
    values = prefix;
    values.push(a.acquire(y1).unwrap());
    values.push(b.acquire(y2).unwrap());
    assert!(capture(&values, &p).is_ok());
    values.push(b.acquire(y3).unwrap());
    assert_eq!(
        capture(&values, &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
}

#[test]
#[ignore = "repeatable resource snapshot CPU cost comparison"]
fn snapshot_capture_cost() {
    use std::{hint::black_box, time::Instant};
    let p = pool();
    let mut registry = ResourceRegistry::new(2048, 4 << 20).unwrap();
    // Real blob-backed image/font identities exercise the ordinary dialogue and
    // multi-image paths; byte size is irrelevant to identity lookup itself.
    let all: Vec<_> = (0..1024)
        .map(|i| {
            let blob = Blob::new(Arc::new([i as u8; 4]));
            let data = if i % 4 == 0 {
                ResourceData::Font(blob)
            } else {
                ResourceData::Image {
                    image: ImageData {
                        data: blob,
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: 1,
                        height: 1,
                    },
                    encoding: ColorEncoding::Srgb,
                }
            };
            let h = registry
                .register(version(i), data, p.reserve(4).unwrap())
                .unwrap();
            registry.acquire(h).unwrap()
        })
        .collect();
    for (n, iterations) in [(8, 10000), (16, 8000), (32, 5000), (256, 500), (1024, 80)] {
        for order in ["forward", "reverse", "mixed"] {
            let values: Vec<_> = (0..n)
                .map(|i| {
                    let k = match order {
                        "reverse" => n - i - 1,
                        "mixed" => (i * 131) % n,
                        _ => i,
                    };
                    all[k].clone()
                })
                .collect();
            let time = |f: fn(
                &[ResourceLease],
                &BudgetPool,
            ) -> Result<ResourceSnapshot, ResourceError>| {
                let start = Instant::now();
                for _ in 0..iterations {
                    black_box(f(black_box(&values), &p).unwrap());
                }
                start.elapsed()
            };
            let old = time(linear);
            let new = time(capture);
            println!(
                "snapshot n={n} order={order} iterations={iterations} old_ns={} new_ns={} ticket_bytes={} extra_index_bytes=0",
                old.as_nanos(),
                new.as_nanos(),
                n * size_of::<ResourceLease>()
            );
        }
    }
}

#[test]
fn image_pixel_reader_retains_original_storage_after_resource_retirement() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register_image(
            version(999),
            OwnedBytes::new(vec![17; 16], pool.reserve(16).unwrap()).unwrap(),
            [2, 2],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let reader = lease.read_pixels().unwrap();
    assert!(registry.retire(handle));
    drop(lease);
    drop(registry);
    assert_eq!(pool.usage().bytes, 16);
    assert_eq!(reader.bytes(), &[17; 16]);
    drop(reader);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn reloadable_pixels_release_only_after_readers_and_restore_identity() {
    let p = pool();
    let mut first = ResourceRegistry::new(8, 1 << 20).unwrap();
    let mut second = ResourceRegistry::new(8, 1 << 20).unwrap();
    let register = |registry: &mut ResourceRegistry, fill| {
        registry
            .register_image(
                version(42),
                OwnedBytes::new(vec![fill; 16], p.reserve(16).unwrap()).unwrap(),
                [2, 2],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap()
    };
    let handle = register(&mut first, 7);
    let image = first.acquire(handle).unwrap();
    assert!(
        !image.discard_uploaded_pixels(),
        "generated content remains resident"
    );
    image.permit_pixel_reload();
    let reader = image.read_pixels().unwrap();
    let id = reader.blob().id();
    assert!(
        !image.discard_uploaded_pixels(),
        "active reader owns pixel bytes"
    );
    drop(reader);
    assert!(image.discard_uploaded_pixels());
    assert_eq!(p.usage().bytes, 0);
    assert_eq!(
        image
            .0
            .pixels
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .weak_storage
            .as_ref()
            .unwrap()
            .bytes(),
        (size_of::<AccountedBlob>() + 2 * size_of::<usize>()) as u64,
        "the dropped producer object stays allocated with its weak identity"
    );
    assert!(image.read_pixels().is_none());
    assert_eq!(image.image_size(), Some([2, 2]));
    let wrong_handle = second
        .register_image(
            version(43),
            OwnedBytes::new(vec![7; 16], p.reserve(16).unwrap()).unwrap(),
            [2, 2],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let wrong = second.acquire(wrong_handle).unwrap();
    assert_eq!(
        image.restore_pixels(&wrong),
        Err(ResourceError::ConflictingVersion)
    );
    assert!(image.read_pixels().is_none());
    second.release(wrong_handle);
    drop(wrong);
    let replacement_handle = register(&mut second, 7);
    let replacement = second.acquire(replacement_handle).unwrap();
    image.restore_pixels(&replacement).unwrap();
    let restored = image.read_pixels().unwrap();
    assert_eq!(restored.bytes(), &[7; 16]);
    assert_eq!(restored.blob().id(), id);
    assert_eq!(image.handle(), handle);
    assert!(
        !image.discard_uploaded_pixels(),
        "restored reader remains live"
    );
}

#[test]
fn atlas_views_with_shared_page_keep_distinct_snapshot_identity() {
    use reel_core::texture::{
        ImageView, TextureDescriptor, TextureFormat, TextureLayout, TexturePayload,
    };
    let p = pool();
    let mut registry = ResourceRegistry::new(32, 1 << 20).unwrap();
    let descriptor = TextureDescriptor {
        storage_layout: Default::default(),
        mip_count: 1,
        format: TextureFormat::Rgba8,
        layout: TextureLayout::identity([128, 8]),
        source_encoding: ColorEncoding::Srgb,
    };
    let bytes = vec![255; 128 * 8 * 4];
    let size = bytes.len() as u64;
    let page = registry
        .register_texture(
            version(0),
            TexturePayload::new(
                descriptor,
                OwnedBytes::new(bytes, p.reserve(size).unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let page = registry.acquire(page).unwrap();
    let mut views = Vec::new();
    for i in 0..20 {
        let image = ImageView {
            logical_size: [4, 4],
            content_origin: [i * 4, 0],
            content_size: [4, 4],
        };
        let h = registry
            .register_atlas_view(
                version(i as usize + 1),
                image,
                ColorEncoding::Srgb,
                page.clone(),
            )
            .unwrap();
        views.push(registry.acquire(h).unwrap());
    }
    let snapshot = capture(&views, &p).unwrap();
    assert_eq!(snapshot.leases().len(), 20);
    assert!(
        snapshot
            .leases()
            .iter()
            .all(|view| view.image_backing().handle() == page.handle())
    );
    assert_eq!(snapshot.leases()[0].image_size(), Some([4, 4]));
    assert_eq!(
        capture(&[views[0].clone(), views[0].clone()], &p).unwrap_err(),
        ResourceError::ConflictingVersion
    );
}

//! Synthetic padded BC3 instances preserve the former regional-image coverage.
use super::*;
use reel_ui::{
    core::WindowEvent,
    dpi::PhysicalSize,
    resources::{ResourceLease, ResourceSnapshot},
};

pub(super) struct RegionalInstances {
    image: ResourceLease,
}

impl RegionalInstances {
    pub(super) fn new(
        registry: &mut ResourceRegistry,
        pool: &BudgetPool,
        handles: &mut Vec<ResourceHandle>,
        versions: &mut Vec<ResourceVersion>,
    ) -> Result<Self> {
        let mut bytes = Vec::with_capacity(128 * 128 * 16);
        for _ in 0..128 {
            for x in 0..128 {
                // Opaque red, transparent content, then opaque green physical padding.
                let alpha = if x < 32 || x >= 96 { 255 } else { 0 };
                bytes.extend_from_slice(&[alpha, alpha, 0, 0, 0, 0, 0, 0]);
                let color = if x < 96 { 0xf800_u16 } else { 0x07e0_u16 };
                bytes.extend_from_slice(&color.to_le_bytes());
                bytes.extend_from_slice(&color.to_le_bytes());
                bytes.extend_from_slice(&[0; 4]);
            }
        }
        let version = ResourceVersion {
            key: AssetKey("device-checks-regional-instances".into()),
            version: ContentVersion([213; 32]),
        };
        let length = bytes.len() as u64;
        let handle = registry
            .register_texture(
                version.clone(),
                TexturePayload::new(
                    TextureDescriptor {
                        storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                        mip_count: 1,
                        format: TextureFormat::Bc3Rgba,
                        layout: TextureLayout {
                            logical_size: [384, 512],
                            content_size: [384, 512],
                            storage_size: [512, 512],
                        },
                        source_encoding: ColorEncoding::Srgb,
                    },
                    OwnedBytes::new(bytes, pool.reserve(length).map_err(error)?).map_err(error)?,
                )?,
            )
            .map_err(error)?;
        handles.push(handle);
        versions.push(version);
        Ok(Self {
            image: registry.acquire(handle).map_err(error)?,
        })
    }

    pub(super) fn frame(&self, index: usize, pool: &BudgetPool) -> Result<RenderFrame> {
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::from_rgb8(20, 34, 55)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 960., 544.)),
            composite: Composite::default(),
        });
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        let phase = (index % 120) as f64 / 60.;
        let offset = 16. * if phase <= 1. { phase } else { 2. - phase };
        for i in 0..24 {
            widget.resource_draws.push((
                self.image.clone(),
                Rect::new(0., 0., 384., 512.),
                Affine::translate((
                    20. + (i % 8) as f64 * 112. + offset,
                    20. + (i / 8) as f64 * 160.,
                )) * Affine::scale_non_uniform(190. / 384., 254. / 512.),
                Default::default(),
            ));
        }
        let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
        bridge.handle_window_event(WindowEvent::Resize(PhysicalSize::new(SIZE[0], SIZE[1])));
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index as u64 + 1)).map_err(error)?,
            ResourceSnapshot::capture([Ok(self.image.clone())].into_iter(), pool).map_err(error)?,
            FrameInfo {
                logical_size: SIZE.map(f64::from),
                physical_size: SIZE,
            },
            r01_fixture::limits(),
            pool,
        )
        .map_err(error)?;
        let mut budget = r01_fixture::step_budget();
        loop {
            let progress = build.step(budget).map_err(error)?;
            if progress.complete {
                break;
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
        build.finish().map_err(error)
    }
}

pub(super) fn check_pixels(pixels: &[u8]) -> Result {
    if pixels.len() != (SIZE[0] * SIZE[1] * 4) as usize {
        return Err("regional image capture size".into());
    }
    let background = [20_u8, 34, 55].map(|v| {
        let v = f64::from(v) / 255.;
        let linear = if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        };
        (linear * 255.).round() as u8
    });
    for (x, y, expected) in [
        (50, 50, [255, 0, 0, 255]),
        (0, 0, [background[0], background[1], background[2], 255]),
        (110, 50, [background[0], background[1], background[2], 255]),
    ] {
        let offset = (y * SIZE[0] + x) as usize * 4;
        if pixels[offset..offset + 4]
            .iter()
            .zip(expected)
            .any(|(a, e)| a.abs_diff(e) > 1)
        {
            return Err(format!(
                "regional instances pixel=({x},{y}) actual={:?} expected={expected:?}",
                &pixels[offset..offset + 4]
            )
            .into());
        }
    }
    Ok(())
}

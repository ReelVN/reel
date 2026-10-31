//! Exercise the actual image paint shaders against an independent color oracle.
use super::{DeviceBackend, Error, Result, color_capture::capture, log};
use std::sync::Arc;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion, SurfaceEpoch},
    texture::{TextureDescriptor, TextureFormat, TextureLayout, TexturePayload},
};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, PresentationBridge},
    core::*,
    dpi::PhysicalSize,
    frame::{
        BuildLimits, FrameBuild, FrameInfo, ImageAlphaLayout, ImageColorAdjustment, RenderFrame,
        StepBudget,
    },
    kurbo::Rect,
    peniko::Color,
    resources::{ColorEncoding, ResourceLease, ResourceRegistry, ResourceSnapshot},
    widgets::Image,
};

const SIZE: [u32; 2] = [16, 8];
fn error(value: impl std::fmt::Debug) -> Error {
    format!("{value:?}").into()
}
fn frame(
    lease: ResourceLease,
    pool: &BudgetPool,
    adjustment: ImageColorAdjustment,
    packed: bool,
    crop: Option<Rect>,
) -> Result<RenderFrame> {
    let widget = Image::from_resource(lease.clone())
        .with_color(Color::from_rgba8(210, 150, 90, 128))
        .with_color_adjustment(adjustment)
        .with_alpha_layout(if packed {
            ImageAlphaLayout::TopBottom
        } else {
            ImageAlphaLayout::Native
        })
        .with_source_rect(crop);
    let mut bridge = PresentationBridge::new(
        RenderRoot::new(
            widget.prepare(),
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(SIZE[0], SIZE[1]),
                scale_factor: 1.,
                default_font: None,
            },
        ),
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 2,
            max_bridge_bytes: 65536,
            max_snapshot_bytes: 4 << 20,
        },
    )
    .map_err(error)?;
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).map_err(error)?,
        ResourceSnapshot::capture([Ok(lease)].into_iter(), pool).map_err(error)?,
        FrameInfo {
            logical_size: SIZE.map(f64::from),
            physical_size: SIZE,
        },
        BuildLimits { max_bytes: 1 << 20 },
        pool,
    )
    .map_err(error)?;
    while !build
        .step(StepBudget {
            max_commands: 128,
            max_units: 65536,
            max_bytes: 1 << 20,
        })
        .map_err(error)?
        .complete
    {}
    build.finish().map_err(error)
}
fn decode(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn encode(value: f64) -> f64 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}
pub(super) fn run(renderer: &mut DeviceBackend) -> Result {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 8 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 1 << 20).map_err(error)?;
    let bytes = vec![
        0, 0, 0, 0, 48, 16, 32, 64, 16, 100, 48, 128, 200, 160, 80, 255, 0, 0, 0, 255, 64, 64, 64,
        255, 128, 128, 128, 255, 255, 255, 255, 255,
    ];
    let ticket = pool.reserve(bytes.capacity() as u64).map_err(error)?;
    let handle = registry
        .register_texture(
            ResourceVersion {
                key: AssetKey("image-color-oracle".into()),
                version: ContentVersion([7; 32]),
            },
            TexturePayload::new(
                TextureDescriptor {
                    storage_layout: Default::default(),
                    mip_count: 1,
                    format: TextureFormat::Rgba8,
                    source_encoding: ColorEncoding::Linear,
                    layout: TextureLayout::identity([4, 2]),
                },
                OwnedBytes::new(bytes, ticket).map_err(error)?,
            )
            .map_err(error)?,
        )
        .map_err(error)?;
    let mut cases = 0;
    let mut max_error = 0_u8;
    for packed in [false, true] {
        for crop in [None, Some(Rect::new(-1., 0., 5., 1.))] {
            let base = capture(
                renderer,
                frame(
                    registry.acquire(handle).map_err(error)?,
                    &pool,
                    ImageColorAdjustment::IDENTITY,
                    packed,
                    crop,
                )?,
                SIZE,
            )?;
            if base.len() != SIZE[0] as usize * SIZE[1] as usize * 4
                || !base
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[3] > 24 && p[3] < 104)
            {
                return Err("image color oracle has no nonzero intermediate alpha".into());
            }
            for adjustment in [
                ImageColorAdjustment {
                    saturation: 3.25,
                    gain: [0.; 3],
                    offset: [-0.2, 0.03, 1.2],
                },
                ImageColorAdjustment {
                    saturation: 0.4,
                    gain: [0.; 3],
                    offset: [0.37, 0.62, 0.9],
                },
                ImageColorAdjustment {
                    saturation: 0.4,
                    gain: [0.8, 0.9, 1.2],
                    offset: [0.03, -0.01, 0.07],
                },
            ] {
                let actual = capture(
                    renderer,
                    frame(
                        registry.acquire(handle).map_err(error)?,
                        &pool,
                        adjustment,
                        packed,
                        crop,
                    )?,
                    SIZE,
                )?;
                if actual.len() != base.len() {
                    return Err("image color size mismatch".into());
                }
                for (pixel, base) in actual
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(base.as_chunks::<4>().0)
                {
                    let alpha = f64::from(base[3]) / 255.;
                    let rgb: [f64; 3] = std::array::from_fn(|c| {
                        if alpha > 0. {
                            encode(f64::from(base[c]) / 255. / alpha)
                        } else {
                            0.
                        }
                    });
                    let gray = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
                    for c in 0..4 {
                        let expected = if c == 3 {
                            base[3]
                        } else {
                            let adjusted = ((gray
                                + f64::from(adjustment.saturation) * (rgb[c] - gray))
                                * f64::from(adjustment.gain[c])
                                + f64::from(adjustment.offset[c]))
                            .clamp(0., 1.);
                            (decode(adjusted) * alpha * 255.).round() as u8
                        };
                        let distance = pixel[c].abs_diff(expected);
                        max_error = max_error.max(distance);
                        if distance > 3 {
                            return Err(format!("image color oracle packed={packed} crop={crop:?} adjustment={adjustment:?} base={base:?} actual={pixel:?} channel={c} expected={expected}").into());
                        }
                    }
                }
                cases += 1;
            }
        }
    }
    log(format_args!(
        "IMAGE COLOR cases={cases} max_error={max_error} tolerance=3 encoding=linear"
    ));
    Ok(())
}

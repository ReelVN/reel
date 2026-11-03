//! Device readback oracle for planar and direct NV12 color conversion.
use super::{DeviceBackend, Error, Result, log, r01_fixture};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
    image::{Yuv420Image, Yuv420Layout, Yuv420Storage, YuvColor, YuvMatrix, YuvTransfer},
    wake::WakeSignal,
};
use reel_render::RenderBackend;
use reel_render::capture::{CaptureBackend, CaptureEncoding};
use reel_ui::{
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    resources::ResourceRegistry,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SAMPLES: [[u8; 3]; 16] = [
    [0, 128, 128],
    [16, 128, 128],
    [23, 128, 128],
    [34, 128, 128],
    [64, 128, 128],
    [128, 128, 128],
    [235, 128, 128],
    [255, 128, 128],
    [81, 90, 240],
    [145, 54, 34],
    [41, 240, 110],
    [128, 16, 240],
    [128, 240, 16],
    [180, 100, 155],
    [32, 150, 120],
    [220, 140, 100],
];
const SIZE: [u32; 2] = [64, 16];

fn error(value: impl std::fmt::Debug) -> Error {
    format!("{value:?}").into()
}

#[derive(Debug)]
struct Planes(Vec<OwnedBytes>);
impl Yuv420Storage for Planes {
    fn plane(&self, index: usize) -> &[u8] {
        self.0[index].as_slice()
    }
}

fn frame(pool: &BudgetPool, color: YuvColor, layout: Yuv420Layout, id: u8) -> Result<RenderFrame> {
    let mut y = vec![0; 32 * 8];
    let mut u = vec![0; 16 * 4];
    let mut v = vec![0; 16 * 4];
    for row in 0..8 {
        for col in 0..32 {
            let sample = SAMPLES[(row / 4) * 8 + col / 4];
            y[row * 32 + col] = sample[0];
            u[(row / 2) * 16 + col / 2] = sample[1];
            v[(row / 2) * 16 + col / 2] = sample[2];
        }
    }
    let (planes, strides) = if layout == Yuv420Layout::Nv12 {
        let uv = u.into_iter().zip(v).flat_map(|(u, v)| [u, v]).collect();
        (vec![y, uv], [32, 32, 0])
    } else {
        (vec![y, u, v], [32, 16, 16])
    };
    let owned = planes
        .into_iter()
        .map(|bytes| {
            let ticket = pool.reserve(bytes.capacity() as u64).map_err(error)?;
            OwnedBytes::new(bytes, ticket).map_err(error)
        })
        .collect::<Result<Vec<_>>>()?;
    let image = Yuv420Image::with_layout([32, 8], color, layout, strides, Arc::new(Planes(owned)))
        .map_err(error)?;
    let mut registry = ResourceRegistry::new(4, 1 << 20).map_err(error)?;
    let handle = registry
        .register_yuv420(
            ResourceVersion {
                key: AssetKey(format!("video-color-{id}")),
                version: ContentVersion([id; 32]),
            },
            image,
        )
        .map_err(error)?;
    let mut widget = r01_fixture::RecordWidget::new(Scene::new());
    widget.resource_draws.push((
        registry.acquire(handle).map_err(error)?,
        Rect::new(0., 0., 32., 8.),
        Affine::scale(2.),
        Default::default(),
    ));
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(id.into())).map_err(error)?,
        registry.freeze(&[handle], pool).map_err(error)?,
        FrameInfo {
            logical_size: [64., 16.],
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

// Independent f64 reference: range expansion, Kr/Kb YCbCr matrix, inverse OETF.
fn expected(sample: [u8; 3], color: YuvColor) -> [u8; 3] {
    let [y, cb, cr] = sample.map(f64::from);
    let (y, cb, cr) = if color.full_range {
        (y / 255., (cb - 128.) / 255., (cr - 128.) / 255.)
    } else {
        ((y - 16.) / 219., (cb - 128.) / 224., (cr - 128.) / 224.)
    };
    let (kr, kb) = match color.matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
    };
    let r = y + 2. * (1. - kr) * cr;
    let b = y + 2. * (1. - kb) * cb;
    let g = (y - kr * r - kb * b) / (1. - kr - kb);
    [r, g, b].map(|encoded| {
        let encoded = encoded.clamp(0., 1.);
        let linear = match color.transfer {
            YuvTransfer::Bt709 if encoded <= 0.081 => encoded / 4.5,
            YuvTransfer::Bt709 => ((encoded + 0.099) / 1.099).powf(1. / 0.45),
            YuvTransfer::Srgb if encoded <= 0.04045 => encoded / 12.92,
            YuvTransfer::Srgb => ((encoded + 0.055) / 1.055).powf(2.4),
        };
        (linear * 255.).round() as u8
    })
}

pub(super) fn run(renderer: &mut DeviceBackend) -> Result {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 128,
            max_bytes: 4 << 20,
        },
    );
    let mut id = 0;
    let mut max_error = 0;
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
        for full_range in [false, true] {
            for transfer in [YuvTransfer::Bt709, YuvTransfer::Srgb] {
                let color = YuvColor {
                    matrix,
                    full_range,
                    transfer,
                };
                for layout in [Yuv420Layout::Planar, Yuv420Layout::Nv12] {
                    id += 1;
                    let frame = frame(&pool, color, layout, id)?;
                    let mut capture =
                        renderer.capture(&frame, SIZE, &pool, WakeSignal::new(|| {}))?;
                    let start = Instant::now();
                    let pixels = loop {
                        if let Some(pixels) = renderer.poll_capture(&mut capture, 4096)? {
                            break pixels;
                        }
                        if start.elapsed() > Duration::from_secs(10) {
                            return Err("video color capture timeout".into());
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    };
                    if pixels.encoding != CaptureEncoding::Linear {
                        return Err("expected linear GXM readback".into());
                    }
                    for (index, sample) in SAMPLES.into_iter().enumerate() {
                        let offset =
                            ((index / 8 * 8 + 4) * SIZE[0] as usize + index % 8 * 8 + 4) * 4;
                        let actual = &pixels.rgba.as_slice()[offset..offset + 4];
                        let expected = expected(sample, color);
                        for channel in 0..3 {
                            let distance = actual[channel].abs_diff(expected[channel]);
                            max_error = max_error.max(distance);
                            if distance > 2 || actual[3] != 255 {
                                return Err(format!("video color {color:?} {layout:?} sample={sample:?} actual={actual:?} expected={expected:?}").into());
                            }
                        }
                    }
                    drop((pixels, capture, frame));
                    renderer.poll(&mut Vec::new(), 32)?;
                }
            }
        }
    }
    log(format_args!(
        "VIDEO COLOR captures={id} samples={} max_linear_byte_error={max_error}",
        usize::from(id) * SAMPLES.len()
    ));
    Ok(())
}

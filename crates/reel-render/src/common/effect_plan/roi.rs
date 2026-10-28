//! Backward source demand on the unchanged effect raster and shader ABI.
use super::*;
use crate::common::scissor::Scissor;
use reel_ui::{frame::FrameGroup, resources::EffectSourceFootprint};
#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "wgpu"))]
pub(crate) mod testing {
    use super::*;
    pub type Invocation = ([u8; 32], [u32; 2], Scissor);
    std::thread_local! {
        static TRACE: std::cell::RefCell<Option<Vec<Invocation>>> = const { std::cell::RefCell::new(None) };
    }
    pub fn observe(id: [u8; 32], size: [u32; 2], scissor: Scissor) {
        TRACE.with_borrow_mut(|trace| {
            if let Some(trace) = trace {
                trace.push((id, size, scissor));
            }
        });
    }
    pub fn collect<T>(run: impl FnOnce() -> T) -> (T, Vec<Invocation>) {
        TRACE.with_borrow_mut(|trace| {
            assert!(trace.is_none());
            *trace = Some(Vec::new());
        });
        let result = run();
        (result, TRACE.with_borrow_mut(|trace| trace.take().unwrap()))
    }
}

/// Reduce source fragment work without changing viewport, storage or sampling.
/// The ordinary color effect is this isolated source's only consumer. Other
/// consumers keep the original domain; enclosing streams supply their own demand.
pub(crate) fn source_scissor(
    drawing: &FrameDrawing,
    group: &FrameGroup,
    to_output: Affine,
    output_size: [u32; 2],
    output: Scissor,
    source_size: [u32; 2],
    source_storage: [u32; 2],
) -> Scissor {
    let full = Scissor::full(source_size);
    if group.mask_effect.is_some()
        || group.clip.is_some()
        || group.mask.is_some()
        || group.image_mask.is_some()
        || !group.filters.is_empty()
    {
        return full;
    }
    let Some(effect) = &group.effect else {
        return full;
    };
    let footprint = drawing.resources().leases().iter().find_map(|lease| {
        if lease.handle() != effect.shader {
            return None;
        }
        match lease.view() {
            ResourceView::EffectShader { descriptor, .. } => descriptor.source_footprint,
            _ => None,
        }
    });
    let Some(footprint) = footprint else {
        return full;
    };
    let Some(mut context) = context(effect, to_output, output_size, source_size) else {
        return full;
    };
    map_storage(&mut context, source_size, source_storage);
    required(&context, footprint, output, source_size).unwrap_or(full)
}

/// Work only with the actual uploaded f32 ABI rows. Whole output pixel cells
/// enclose sample positions (including MSAA); the fullscreen TEXCOORD path keeps
/// its original viewport and interpolation. Ill-conditioned arithmetic retains
/// the full source instead of inferring coverage from an arbitrary epsilon.
fn required(
    context: &[[f32; 4]; crate::effect::CONTEXT_SLOTS],
    footprint: EffectSourceFootprint,
    output: Scissor,
    size: [u32; 2],
) -> Option<Scissor> {
    if output.empty() {
        return Some(Scissor {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
    }
    if context[5][..2]
        .iter()
        .any(|&n| !(1.0..=65536.0).contains(&n))
        || size.contains(&0)
    {
        return None;
    }
    let pixels = [
        [f64::from(output.x), f64::from(output.x + output.width)],
        [f64::from(output.y), f64::from(output.y + output.height)],
    ];
    let mut uv = [
        dot(context[0], pixels, size)?,
        dot(context[1], pixels, size)?,
    ];
    // The wrapper discards UV outside its original local domain. Every source
    // sample then adds one proved offset and clamps again before mapping storage.
    for axis in 0..2 {
        uv[axis] = [uv[axis][0].max(0.), uv[axis][1].min(1.)];
        if uv[axis][0] > uv[axis][1] {
            return Some(Scissor {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
        }
        let logical = f64::from(context[4][axis + 2]);
        if !(logical > 0. && logical.is_finite()) {
            return None;
        }
        let lo = f64::from(footprint.logical_halo[axis]) / logical;
        let hi = f64::from(footprint.logical_halo[axis + 2]) / logical;
        // One division and one addition, on original logical Size. This also
        // encloses contraction; constant arithmetic was proved at compile time.
        let error = gamma(2) * (1. + lo.max(hi));
        uv[axis] = [
            (uv[axis][0] - lo - error).clamp(0., 1.),
            (uv[axis][1] + hi + error).clamp(0., 1.),
        ];
    }
    let q = [dot(context[2], uv, size)?, dot(context[3], uv, size)?];
    let mut edges = [[0; 2]; 2];
    for axis in 0..2 {
        let storage = f64::from(context[5][axis + 2]);
        if !(storage >= f64::from(size[axis]) && storage.is_finite()) {
            return None;
        }
        // Source sampling is base-level linear filtering. Include both adjacent
        // texels and their outer pixel cells, then intersect the original raster.
        // Unwritten padding remains the same initialized transparent storage.
        let lo = q[axis][0] * storage;
        let hi = q[axis][1] * storage;
        let error = gamma(1) * lo.abs().max(hi.abs());
        edges[axis] = [
            ((lo - error - 0.5).floor() - 1.).clamp(0., f64::from(size[axis])) as u32,
            ((hi + error + 0.5).ceil() + 1.).clamp(0., f64::from(size[axis])) as u32,
        ];
    }
    Some(Scissor {
        x: edges[0][0],
        y: edges[1][0],
        width: edges[0][1].saturating_sub(edges[0][0]),
        height: edges[1][1].saturating_sub(edges[1][0]),
    })
}

/// Standard floating-point forward-error bound for n rounded f32 operations.
fn gamma(n: u32) -> f64 {
    let roundoff = f64::from(f32::EPSILON) * 0.5 * f64::from(n);
    roundoff / (1. - roundoff)
}
fn dot(row: [f32; 4], input: [[f64; 2]; 2], size: [u32; 2]) -> Option<[f64; 2]> {
    let mut range = [f64::from(row[2]); 2];
    let mut magnitude = f64::from(row[2]).abs();
    for axis in 0..2 {
        let products = input[axis].map(|v| v * f64::from(row[axis]));
        range[0] += products[0].min(products[1]);
        range[1] += products[0].max(products[1]);
        magnitude += products[0].abs().max(products[1].abs());
    }
    // Three products/two additions, including the constant lane, independently
    // enclose the ABI dot and any fused or reassociated evaluation.
    let error = gamma(5) * magnitude;
    if !error.is_finite() || error * f64::from(size[0].max(size[1])) >= 0.5 {
        return None;
    }
    Some([range[0] - error, range[1] + error])
}

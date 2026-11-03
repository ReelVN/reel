//! Prove that a regional clamp cannot affect any shaded texture coordinate.
use super::{
    mesh::{DirectPaint, Mesh, Vertex},
    scissor::Scissor,
};

#[cfg(any(feature = "gxm", test, all(feature = "wgpu", feature = "profiling")))]
mod interior;
#[cfg(feature = "gxm")]
pub(crate) use interior::partition;
#[cfg(any(feature = "gxm", all(feature = "wgpu", feature = "profiling")))]
pub(crate) use interior::regional_interior;

/// Keep original UVs and geometry. Removing an inactive fragment clamp permits
/// non-dependent texture reads on GXM and the same direct program on WGPU.
pub(crate) fn direct_paint(
    mesh: &Mesh,
    paint: Option<DirectPaint>,
    texture: [u32; 2],
    viewport: [u32; 2],
    scissor: Scissor,
    mip_count: u32,
) -> Option<DirectPaint> {
    let direct = match paint? {
        DirectPaint::RegionalRgba => DirectPaint::Rgba,
        DirectPaint::RegionalBc1 => DirectPaint::Bc1,
        DirectPaint::RegionalBc3 => DirectPaint::Bc3,
        _ => return paint,
    };
    // A shader clamp can change implicit LOD derivatives at full-texture edges.
    // Single-level textures have no LOD choice to preserve.
    if mip_count != 1 || texture.contains(&0) || viewport.contains(&0) {
        return paint;
    }
    if mesh.indices.as_chunks::<3>().0.iter().all(|indices| {
        triangle(
            std::array::from_fn(|i| &mesh.vertices[indices[i] as usize]),
            texture,
            viewport,
            scissor.intersect(Scissor::full(viewport)),
        )
    }) {
        Some(direct)
    } else {
        paint
    }
}

fn triangle(
    vertices: [&Vertex; 3],
    texture: [u32; 2],
    viewport: [u32; 2],
    scissor: Scissor,
) -> bool {
    let window = vertices[0].source_window;
    if window.iter().any(|v| !v.is_finite())
        || window[2] <= 0.
        || window[3] <= 0.
        || vertices
            .iter()
            .any(|v| v.source_window != window || v.uv.iter().any(|v| !v.is_finite()))
    {
        return false;
    }
    let position = vertices.map(|v| {
        [
            (f64::from(v.position[0]) + 1.) * f64::from(viewport[0]) * 0.5,
            (1. - f64::from(v.position[1])) * f64::from(viewport[1]) * 0.5,
        ]
    });
    if position.iter().flatten().any(|v| !v.is_finite()) {
        return false;
    }
    let dx = [
        position[1][0] - position[0][0],
        position[2][0] - position[0][0],
    ];
    let dy = [
        position[1][1] - position[0][1],
        position[2][1] - position[0][1],
    ];
    let determinant = dx[0] * dy[1] - dx[1] * dy[0];
    for axis in 0..2 {
        let low = f64::from(window[axis]);
        let high = low + f64::from(window[axis + 2]);
        // A full texture axis has exactly the hardware pad sampler's bounds.
        if low <= 0. && high >= 1. {
            continue;
        }
        let uv = vertices.map(|v| f64::from(v.uv[axis]));
        let delta = [uv[1] - uv[0], uv[2] - uv[0]];
        let gradient = [
            (delta[0] * dy[1] - delta[1] * dy[0]) / determinant,
            (dx[0] * delta[1] - dx[1] * delta[0]) / determinant,
        ];
        // Shading and coverage locations can differ within one physical pixel.
        // Bound both axes, including rotation, shear and minification. The fp32
        // guard keeps interpolation roundoff away from the inactive clamp.
        let border = gradient[0].abs() + gradient[1].abs() + 8. * f64::from(f32::EPSILON);
        let inset = (0.5 / f64::from(texture[axis])).min((high - low) * 0.5);
        if !border.is_finite() {
            return false;
        }
        if uv
            .iter()
            .all(|v| *v >= low + inset + border && *v <= high - inset - border)
        {
            continue;
        }
        // The intersection of the triangle's bounding box and the actual
        // scissor contains every visible coverage location. Expand each by one
        // physical pixel for raster quantization and extrapolated shading.
        // Affine UV extrema on this larger rectangle bound the triangle too;
        // no clipping geometry or scratch allocation is needed.
        let range = std::array::from_fn::<_, 2, _>(|dimension| {
            let origin = f64::from(if dimension == 0 { scissor.x } else { scissor.y });
            let extent = f64::from(if dimension == 0 {
                scissor.width
            } else {
                scissor.height
            });
            [
                position
                    .iter()
                    .map(|p| p[dimension])
                    .fold(f64::INFINITY, f64::min)
                    .max(origin)
                    - 1.,
                position
                    .iter()
                    .map(|p| p[dimension])
                    .fold(f64::NEG_INFINITY, f64::max)
                    .min(origin + extent)
                    + 1.,
            ]
        });
        if range.iter().any(|r| r[0] > r[1]) {
            continue;
        }
        let mut bounds = [uv[0]; 2];
        let mut magnitude = uv[0].abs();
        for dimension in 0..2 {
            let terms =
                range[dimension].map(|p| gradient[dimension] * (p - position[0][dimension]));
            bounds[0] += terms[0].min(terms[1]);
            bounds[1] += terms[0].max(terms[1]);
            magnitude += terms[0].abs().max(terms[1].abs());
        }
        let error = 8. * f64::from(f32::EPSILON) * magnitude.max(1.);
        if !bounds.iter().all(|v| v.is_finite())
            || bounds[0] < low + inset + error
            || bounds[1] > high - inset - error
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "sampling_domain_tests.rs"]
mod tests;

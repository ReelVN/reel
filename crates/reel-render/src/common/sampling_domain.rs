//! Prove that a regional clamp cannot affect any shaded texture coordinate.
use super::mesh::{DirectPaint, Mesh, Vertex};

/// Keep original UVs and geometry. Removing an inactive fragment clamp permits
/// non-dependent texture reads on GXM and the same direct program on WGPU.
pub(crate) fn direct_paint(
    mesh: &Mesh,
    paint: Option<DirectPaint>,
    texture: [u32; 2],
    viewport: [u32; 2],
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
    if mesh.indices.chunks_exact(3).all(|indices| {
        triangle(
            std::array::from_fn(|i| &mesh.vertices[indices[i] as usize]),
            texture,
            viewport,
        )
    }) {
        Some(direct)
    } else {
        paint
    }
}

fn triangle(vertices: [&Vertex; 3], texture: [u32; 2], viewport: [u32; 2]) -> bool {
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
        std::array::from_fn::<_, 2, _>(|axis| {
            f64::from(v.position[axis]) * f64::from(viewport[axis]) * 0.5
        })
    });
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
        if !border.is_finite()
            || uv
                .iter()
                .any(|v| !v.is_finite() || *v < low + inset + border || *v > high - inset - border)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "sampling_domain_tests.rs"]
mod tests;

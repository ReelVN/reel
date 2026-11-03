//! Keep atlas edge clamps on border tiles while sampling the interior directly.
use super::{DirectPaint, Mesh, Scissor, direct_paint};

/// Find a large tile-aligned interior of an axis-aligned textured mesh.
/// All draws retain the original vertices, indices and interpolation. The final
/// domain proof also covers quantized coverage and extrapolated shading points.
pub(crate) fn regional_interior(
    mesh: &Mesh,
    texture: [u32; 2],
    viewport: [u32; 2],
    scissor: Scissor,
    mip_count: u32,
) -> Option<Scissor> {
    if mip_count != 1
        || texture.contains(&0)
        || viewport.contains(&0)
        || mesh.vertices.len() < 3
        || mesh.indices.is_empty()
    {
        return None;
    }
    let window = mesh.vertices[0].source_window;
    if window.iter().any(|v| !v.is_finite())
        || window[2] <= 0.
        || window[3] <= 0.
        || mesh.vertices.iter().any(|v| v.source_window != window)
    {
        return None;
    }
    let bounds = scissor.intersect(Scissor::full(viewport));
    let mut low = [bounds.x, bounds.y];
    let mut high = [bounds.x + bounds.width, bounds.y + bounds.height];
    for axis in 0..2 {
        let position = |vertex: &super::super::mesh::Vertex| {
            let value = f64::from(vertex.position[axis]);
            (if axis == 0 { value + 1. } else { 1. - value }) * f64::from(viewport[axis]) * 0.5
        };
        if mesh
            .vertices
            .iter()
            .any(|vertex| !position(vertex).is_finite())
        {
            return None;
        }
        let first = mesh
            .vertices
            .iter()
            .map(position)
            .fold(f64::INFINITY, f64::min);
        let last = mesh
            .vertices
            .iter()
            .map(position)
            .fold(f64::NEG_INFINITY, f64::max);
        if first >= last {
            return None;
        }
        let start = mesh.vertices.iter().find(|v| position(v) == first)?.uv[axis];
        let end = mesh.vertices.iter().find(|v| position(v) == last)?.uv[axis];
        if !start.is_finite() || !end.is_finite() || start == end {
            return None;
        }
        let gradient = (f64::from(end) - f64::from(start)) / (last - first);
        // Image coverage builds several disjoint quads with the same UV map.
        // Use their common map only to propose a clip; the final domain proof
        // validates every original triangle, including fp32 interpolation error.
        if mesh.vertices.iter().any(|vertex| {
            let expected = f64::from(start) + (position(vertex) - first) * gradient;
            let actual = f64::from(vertex.uv[axis]);
            !actual.is_finite()
                || (expected - actual).abs()
                    > 8. * f64::from(f32::EPSILON) * expected.abs().max(actual.abs()).max(1.)
        }) {
            return None;
        }
        let half = 0.5 / f64::from(texture[axis]);
        let span = f64::from(window[axis + 2]);
        let inset = half.min(span * 0.5);
        let minimum = (f64::from(window[axis]) + inset).max(half);
        let maximum = (f64::from(window[axis]) + span - inset).min(1. - half);
        let at = [minimum, maximum].map(|uv| first + (uv - f64::from(start)) / gradient);
        if at.iter().any(|p| !p.is_finite()) {
            return None;
        }
        // Two physical pixels leave room for the final fp32 domain proof; tile
        // alignment then avoids adding a fragment clip to the direct interior.
        low[axis] = low[axis].max((at[0].min(at[1]) + 2.).ceil().max(0.) as u32);
        high[axis] = high[axis].min((at[0].max(at[1]) - 2.).floor().max(0.) as u32);
        low[axis] = low[axis].div_ceil(32).checked_mul(32)?;
        high[axis] = high[axis] / 32 * 32;
    }
    let inner = Scissor {
        x: low[0],
        y: low[1],
        width: high[0].checked_sub(low[0])?,
        height: high[1].checked_sub(low[1])?,
    };
    let pixels = u64::from(inner.width) * u64::from(inner.height);
    // Require a substantial interior before adding up to four draws. Small
    // images and glyphs keep their original single draw.
    if pixels < 65_536 || pixels * 4 < u64::from(bounds.width) * u64::from(bounds.height) * 3 {
        return None;
    }
    (direct_paint(
        mesh,
        Some(DirectPaint::RegionalRgba),
        texture,
        viewport,
        inner,
        1,
    ) == Some(DirectPaint::Rgba))
    .then_some(inner)
}

/// A disjoint cover of the original clip. The first rectangle is the proved
/// direct interior; the following four keep the original regional program.
#[cfg(any(feature = "gxm", test))]
pub(crate) fn partition(outer: Scissor, inner: Scissor) -> [Scissor; 5] {
    [
        inner,
        Scissor {
            height: inner.y - outer.y,
            ..outer
        },
        Scissor {
            x: outer.x,
            y: inner.y,
            width: inner.x - outer.x,
            height: inner.height,
        },
        Scissor {
            x: inner.x + inner.width,
            y: inner.y,
            width: outer.x + outer.width - inner.x - inner.width,
            height: inner.height,
        },
        Scissor {
            y: inner.y + inner.height,
            height: outer.y + outer.height - inner.y - inner.height,
            ..outer
        },
    ]
}

#[cfg(test)]
mod tests;

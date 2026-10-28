//! Piecewise affine source clamping for padded scalar effects.
use crate::common::mesh::Vertex;
use reel_ui::kurbo::{Affine, Point};

/// Split the viewport at the source's first and last texel centers. Each cell
/// has affine clamped UVs, so both backends can interpolate them before sampling.
/// Positions are clip-space; color.xy retains the original top-left target UV.
/// A singular or unrepresentable source mapping has no drawable mesh.
pub(crate) struct Geometry {
    vertices: [Vertex; 16],
    vertex_count: usize,
    indices: [u32; 54],
    index_count: usize,
}
impl Geometry {
    pub(crate) fn vertices(&self) -> &[Vertex] {
        &self.vertices[..self.vertex_count]
    }
    pub(super) fn vertices_mut(&mut self) -> &mut [Vertex] {
        &mut self.vertices[..self.vertex_count]
    }
    pub(crate) fn indices(&self) -> &[u32] {
        &self.indices[..self.index_count]
    }
    #[cfg(test)]
    fn triangles(&self) -> Vec<Vertex> {
        self.indices()
            .iter()
            .map(|&index| self.vertices[index as usize])
            .collect()
    }
}

pub(crate) fn geometry(context: &[[f32; 4]; crate::effect::CONTEXT_SLOTS]) -> Option<Geometry> {
    let x = context[7].map(f64::from);
    let y = context[8].map(f64::from);
    let mapping = Affine::new([x[0], y[0], x[1], y[1], x[2], y[2]]);
    let determinant = mapping.determinant();
    if determinant == 0. || !determinant.is_finite() {
        return None;
    }
    let inverse = mapping.inverse();
    let corners = [
        Point::new(0., 0.),
        Point::new(1., 0.),
        Point::new(0., 1.),
        Point::new(1., 1.),
    ]
    .map(|point| mapping * point);
    let mut centers = [[0.; 2]; 2];
    let mut grid = [[0.; 4]; 2];
    for axis in 0..2 {
        let half = 0.5 / f64::from(context[5][axis + 2]);
        let start = f64::from(context[6][axis]) + half;
        // A one-texel content window has one center, including when its
        // normalized width rounded below one texel in the float32 context.
        let end = (f64::from(context[6][axis]) + f64::from(context[6][axis + 2]) - half).max(start);
        centers[axis] = [start, end];
        let coordinates = corners.map(|point| [point.x, point.y][axis]);
        let low = coordinates.into_iter().fold(f64::INFINITY, f64::min);
        let high = coordinates.into_iter().fold(f64::NEG_INFINITY, f64::max);
        grid[axis] = [low, start.clamp(low, high), end.clamp(low, high), high];
    }
    let points: [Vertex; 16] = std::array::from_fn(|index| {
        let source = Point::new(grid[0][index % 4], grid[1][index / 4]);
        let target = inverse * source;
        Vertex {
            position: [(target.x * 2. - 1.) as f32, (1. - target.y * 2.) as f32],
            uv: [
                source.x.clamp(centers[0][0], centers[0][1]) as f32,
                source.y.clamp(centers[1][0], centers[1][1]) as f32,
            ],
            color: [target.x as f32, target.y as f32, 0., 0.],
            source_window: [0.; 4],
        }
    });
    if points.iter().any(|point| {
        point
            .position
            .iter()
            .chain(&point.uv)
            .chain(&point.color)
            .any(|value| !value.is_finite())
    }) {
        return None;
    }
    let mut mesh = Geometry {
        vertices: points,
        vertex_count: 0,
        indices: [0; 54],
        index_count: 0,
    };
    let mut remap = [u32::MAX; 16];
    for row in 0..3 {
        for column in 0..3 {
            if grid[0][column] == grid[0][column + 1] || grid[1][row] == grid[1][row + 1] {
                continue;
            }
            let first = row * 4 + column;
            for offset in [0, 1, 4, 1, 5, 4] {
                let original = first + offset;
                if remap[original] == u32::MAX {
                    remap[original] = mesh.vertex_count as u32;
                    mesh.vertices[mesh.vertex_count] = points[original];
                    mesh.vertex_count += 1;
                }
                mesh.indices[mesh.index_count] = remap[original];
                mesh.index_count += 1;
            }
        }
    }
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(mapping: Affine, window: [f32; 4]) -> [[f32; 4]; crate::effect::CONTEXT_SLOTS] {
        let [a, b, c, d, e, f] = mapping.as_coeffs().map(|value| value as f32);
        let mut context = [[0.; 4]; crate::effect::CONTEXT_SLOTS];
        context[5] = [960., 544., 512., 256.];
        context[6] = window;
        context[7] = [a, c, e, 0.];
        context[8] = [b, d, f, 0.];
        context
    }

    fn sample(vertices: &[Vertex], target: [f64; 2]) -> ([f64; 2], [f64; 2]) {
        let point = [target[0] * 2. - 1., 1. - target[1] * 2.];
        for triangle in vertices.as_chunks::<3>().0 {
            let a = triangle[0].position.map(f64::from);
            let b = triangle[1].position.map(f64::from);
            let c = triangle[2].position.map(f64::from);
            let determinant = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if determinant.abs() < 1e-14 {
                continue;
            }
            let u = ((point[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (point[1] - a[1]))
                / determinant;
            let v = ((b[0] - a[0]) * (point[1] - a[1]) - (point[0] - a[0]) * (b[1] - a[1]))
                / determinant;
            let weights = [1. - u - v, u, v];
            if weights
                .into_iter()
                .any(|weight| !(-1e-7..=1.0000001).contains(&weight))
            {
                continue;
            }
            let interpolate = |field: fn(&Vertex) -> [f32; 2]| {
                std::array::from_fn(|axis| {
                    weights
                        .iter()
                        .zip(triangle)
                        .map(|(weight, vertex)| weight * f64::from(field(vertex)[axis]))
                        .sum()
                })
            };
            return (
                interpolate(|vertex| vertex.uv),
                interpolate(|vertex| [vertex.color[0], vertex.color[1]]),
            );
        }
        panic!("viewport point {target:?} is not covered");
    }

    #[test]
    fn interpolated_samples_match_window_clamping_through_mirrors_rotations_and_scissors() {
        for mapping in [
            Affine::new([0.8, 0., 0., 0.7, -0.1, 0.2]),
            Affine::translate((0.7, -0.2))
                * Affine::rotate(0.37)
                * Affine::scale_non_uniform(-0.8, 0.9),
            Affine::new([0.6, 0.35, -0.4, 0.7, 0.2, -0.1]),
            Affine::translate((2., -3.)) * Affine::scale_non_uniform(0.2, -0.3),
        ] {
            for window in [
                [0., 0., 0.5, 0.75],
                [0., 0.25, 0.5, 0.75],
                [0.1, 0.2, 1. / 512., 1. / 256.],
            ] {
                let context = context(mapping, window);
                let mesh = geometry(&context).unwrap().triangles();
                // A backend scissor restricts the same complete viewport mesh.
                for bounds in [[0., 0., 1., 1.], [0.17, 0.31, 0.73, 0.64]] {
                    for y in 0..19 {
                        for x in 0..23 {
                            let target = [
                                bounds[0] + (bounds[2] - bounds[0]) * (f64::from(x) + 0.5) / 23.,
                                bounds[1] + (bounds[3] - bounds[1]) * (f64::from(y) + 0.5) / 19.,
                            ];
                            let (sampled, interpolated_target) = sample(&mesh, target);
                            for axis in 0..2 {
                                let row = context[7 + axis].map(f64::from);
                                let raw = row[0] * target[0] + row[1] * target[1] + row[2];
                                let half = 0.5 / f64::from(context[5][axis + 2]);
                                let first = f64::from(window[axis]) + half;
                                let last = (f64::from(window[axis]) + f64::from(window[axis + 2])
                                    - half)
                                    .max(first);
                                assert!(
                                    (sampled[axis] - raw.clamp(first, last)).abs()
                                        * f64::from(context[5][axis + 2])
                                        < 0.0003
                                );
                                assert!(
                                    (interpolated_target[axis] - target[axis]).abs() < 0.000001
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn indexed_cells_reduce_upload_bytes_and_preserve_triangle_attributes() {
        use crate::common::{
            GpuLimits,
            draw_batch::Batch,
            mesh::{Allocation, Mesh},
            renderer::Budgets,
        };
        let limits = GpuLimits::default();
        let global = Budgets::new(limits);
        let local = Budgets::new(limits);
        for (mapping, expected_vertices, expected_indices) in [
            (Affine::IDENTITY, 16, 54),
            (
                Affine::translate((2., -3.)) * Affine::scale_non_uniform(0.2, -0.3),
                4,
                6,
            ),
        ] {
            let context = context(mapping, [0., 0., 0.5, 0.75]);
            let geometry = geometry(&context).unwrap();
            assert_eq!(geometry.vertices().len(), expected_vertices);
            assert_eq!(geometry.indices().len(), expected_indices);
            let mut mesh = Mesh::new(Allocation::new(
                &global,
                &local,
                limits.transient_bytes,
                1 << 20,
            ));
            mesh.indexed_triangles(geometry.vertices(), geometry.indices().iter().copied())
                .unwrap();
            let triangles = geometry.triangles();
            for (index, original) in mesh.indices.iter().zip(triangles) {
                let actual = &mesh.vertices[*index as usize];
                assert_eq!(actual.position, original.position);
                assert_eq!(actual.uv, original.uv);
                assert_eq!(actual.color, original.color);
            }
            let batch = Batch::new(mesh, ()).unwrap();
            let before = 54 * (size_of::<Vertex>() + size_of::<u16>()) as u64;
            let after = batch.capacity().bytes();
            assert_eq!(
                after,
                (expected_vertices * size_of::<Vertex>() + expected_indices * size_of::<u16>())
                    as u64
            );
            println!(
                "padded_effect vertices=54->{expected_vertices} indices=54->{expected_indices} upload_bytes={before}->{after}"
            );
            assert!(after < before);
            drop(batch);
            assert_eq!(global.transient.usage().bytes, 0);
            assert_eq!(local.transient.usage().bytes, 0);
        }
    }

    #[test]
    fn cell_boundaries_share_vertices_and_singular_maps_have_no_mesh() {
        let context = context(Affine::IDENTITY, [0., 0., 0.5, 0.75]);
        let mesh = geometry(&context).unwrap().triangles();
        for row in 0..3 {
            for column in 0..2 {
                let left = (row * 3 + column) * 6;
                let right = left + 6;
                assert_eq!(mesh[left + 1].position, mesh[right].position);
                assert_eq!(mesh[left + 1].uv, mesh[right].uv);
                assert_eq!(mesh[left + 4].position, mesh[right + 2].position);
                assert_eq!(mesh[left + 4].uv, mesh[right + 2].uv);
            }
        }
        let mut singular = context;
        singular[8] = singular[7];
        assert!(geometry(&singular).is_none());
    }
}

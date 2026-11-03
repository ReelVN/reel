//! Frame-local initialization of pooled target uses. Allocation and first write
//! are separate events; sampling an unwritten source materializes transparent pixels.
use super::{GpuError, scopes::push_stack};
mod stamps;
pub(crate) use stamps::Stamp;

/// Two triangles sharing one diagonal of a strictly convex quad have no
/// overlapping covered samples. Source-over on a freshly transparent target
/// therefore equals replacement for this one draw, including translucent color.
/// This proof does not initialize pixels outside the geometry.
#[cfg(any(
    all(feature = "gxm", target_os = "vita"),
    all(feature = "wgpu", feature = "profiling"),
    test
))]
pub(crate) fn single_quad(mesh: &super::mesh::Mesh, viewport: [u32; 2]) -> bool {
    if viewport.contains(&0)
        || mesh.vertices.len() != 4
        || mesh.indices.as_slice() != [0, 1, 2, 0, 2, 3]
    {
        return false;
    }
    let points: [[f64; 2]; 4] = std::array::from_fn(|i| {
        let [x, y] = mesh.vertices[i].position.map(f64::from);
        [
            (x + 1.) * f64::from(viewport[0]) * 0.5,
            (1. - y) * f64::from(viewport[1]) * 0.5,
        ]
    });
    if points
        .iter()
        .flatten()
        .any(|value| !value.is_finite() || value.abs() > 65_536.)
    {
        return false;
    }
    let diagonal = [points[2][0] - points[0][0], points[2][1] - points[0][1]];
    let sides = [1, 3].map(|i| {
        let edge = [points[i][0] - points[0][0], points[i][1] - points[0][1]];
        let cross = diagonal[0] * edge[1] - diagonal[1] * edge[0];
        // One pixel of uncertainty per vertex covers viewport roundoff and
        // subpixel coverage quantization within the admitted coordinate range.
        // Each difference can move by two; the two product errors add eight.
        let error =
            2. * (diagonal[0].abs() + diagonal[1].abs() + edge[0].abs() + edge[1].abs()) + 8.;
        if cross > error {
            1
        } else if cross < -error {
            -1
        } else {
            0
        }
    });
    sides[0] != 0 && sides[0] == -sides[1]
}

/// A fullscreen, non-discarding replacement writes every physical sample.
/// Callers choose a shader with unconditional color output and disabled blending.
/// Pooled padding must be written too, even when no logical node occupies it.
#[derive(Clone, Copy)]
pub(crate) struct FullOverwrite(());
impl FullOverwrite {
    pub(crate) fn composite(
        viewport: [u32; 2],
        storage: [u32; 2],
        scissor: super::scissor::Scissor,
        replaces: bool,
    ) -> Option<Self> {
        (replaces
            && !storage.contains(&0)
            && viewport == storage
            && scissor == super::scissor::Scissor::full(storage))
        .then_some(Self(()))
    }
}

#[derive(Default)]
pub(crate) struct TargetWrites {
    pending: Vec<u64>,
    stamps: stamps::Stamps,
}

impl TargetWrites {
    pub(crate) fn begin(
        &mut self,
        slot: usize,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        // Preallocated targets can reach their first use in any slot order.
        while slot / 64 >= self.pending.len() {
            push_stack(&mut self.pending, 0, &mut reserve)?;
        }
        self.pending[slot / 64] |= 1 << (slot % 64);
        self.stamps.begin(slot);
        Ok(())
    }

    /// Observe the current lease after producing an ordinary, exclusively read
    /// effect result. Later writes must acquire another lease through `begin`.
    pub(crate) fn watch(
        &mut self,
        slot: usize,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Stamp, GpuError> {
        self.stamps.watch(slot, &mut reserve)
    }

    /// Whether this preparation has acquired another writable lease of the owner.
    pub(crate) fn is_current(&self, stamp: Stamp) -> bool {
        self.stamps.is_current(stamp)
    }

    /// Whether this logical target use still needs its first complete initialization.
    pub(crate) fn pending(&self, slot: usize) -> bool {
        self.pending[slot / 64] & (1 << (slot % 64)) != 0
    }

    /// The first writer clears; later writers preserve earlier contents.
    pub(crate) fn take(&mut self, slot: usize) -> bool {
        let word = &mut self.pending[slot / 64];
        let bit = 1 << (slot % 64);
        let clear = *word & bit != 0;
        *word &= !bit;
        clear
    }

    /// Mark the next write and return whether transparent initialization is
    /// required. A proved complete replacement initializes every physical
    /// sample itself, including padding and transparent pixels outside the node.
    pub(crate) fn write(&mut self, slot: usize, overwrite: Option<FullOverwrite>) -> bool {
        self.take(slot) && overwrite.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_quad_proof_rejects_overlapping_and_quantization_ambiguous_triangles() {
        use super::super::{
            mesh::{Allocation, Mesh},
            renderer::Budgets,
        };
        use reel_ui::kurbo::Rect;
        let shared = Budgets::new(Default::default());
        let local = Budgets::new(Default::default());
        let mut mesh = Mesh::new(Allocation::new(&shared, &local, 1 << 20, 1 << 20));
        mesh.quad(Rect::new(-0.8, -0.8, 0.8, 0.8), Rect::ZERO)
            .unwrap();
        let original: Vec<_> = mesh.vertices.iter().map(|v| v.position).collect();
        for angle in [0.0f32, 0.3, 1.7, 3.0] {
            for mirror in [-1., 1.] {
                for (v, [x, y]) in mesh.vertices.iter_mut().zip(&original) {
                    v.position = [
                        mirror * (x * angle.cos() - y * angle.sin()),
                        x * angle.sin() + y * angle.cos(),
                    ];
                }
                assert!(single_quad(&mesh, [960, 544]));
            }
        }
        for (v, p) in mesh.vertices.iter_mut().zip(&original) {
            v.position = *p;
        }
        mesh.vertices[3].position = mesh.vertices[1].position;
        assert!(!single_quad(&mesh, [960, 544]), "triangles overlap");
        mesh.vertices[3].position = [0., 0.00001];
        assert!(
            !single_quad(&mesh, [960, 544]),
            "rounding can cross the shared diagonal"
        );
        mesh.vertices[3].position = original[3];
        mesh.indices.extend([0, 1, 2]);
        assert!(
            !single_quad(&mesh, [960, 544]),
            "a second paint must retain blending"
        );
        mesh.indices.truncate(6);
        mesh.vertices[3].position = [f32::NAN, 0.];
        assert!(!single_quad(&mesh, [960, 544]));
        mesh.vertices[3].position = original[3];
        assert!(!single_quad(&mesh, [0, 544]));
    }

    #[test]
    fn preallocated_slots_can_begin_out_of_order_without_initializing_the_gaps() {
        let mut writes = TargetWrites::default();
        writes.begin(129, |_| Ok(())).unwrap();
        assert!(writes.take(129));
        assert!(!writes.take(128));
        assert!(!writes.take(64));
        writes.begin(0, |_| Ok(())).unwrap();
        writes.begin(64, |_| Ok(())).unwrap();
        assert!(writes.take(64));
        assert!(writes.take(0));
        assert!(!writes.take(129));
    }

    #[test]
    fn overwrite_requires_the_whole_physical_surface_and_replacement() {
        use super::super::scissor::Scissor;
        let size = [65, 11];
        let full = Scissor::full(size);
        assert!(FullOverwrite::composite(size, size, full, true).is_some());
        assert!(FullOverwrite::composite(size, size, full, false).is_none());
        assert!(FullOverwrite::composite(size, [128, 16], full, true).is_none());
        assert!(FullOverwrite::composite([0, 0], [0, 0], Scissor::full([0, 0]), true).is_none());
        for scissor in [
            Scissor {
                x: 1,
                width: 64,
                ..full
            },
            Scissor { height: 10, ..full },
            Scissor::full([0, 0]),
        ] {
            assert!(FullOverwrite::composite(size, size, scissor, true).is_none());
        }
    }

    #[test]
    fn replacement_initializes_a_reused_slot_and_partial_writes_still_clear() {
        use super::super::scissor::Scissor;
        let size = [65, 11];
        let mut writes = TargetWrites::default();
        writes.begin(0, |_| Ok(())).unwrap();
        let complete = FullOverwrite::composite(size, size, Scissor::full(size), true);
        assert!(!writes.write(0, complete));
        assert!(!writes.pending(0));
        assert!(!writes.write(0, None), "preserve the preceding full output");
        writes.begin(0, |_| Ok(())).unwrap();
        let partial = FullOverwrite::composite(size, [128, 16], Scissor::full(size), true);
        assert!(
            writes.write(0, partial),
            "physical padding still needs initialization"
        );
        assert!(!writes.write(0, None));
    }

    #[test]
    fn pooled_uses_initialize_once_and_preparations_keep_separate_state() {
        let mut first = TargetWrites::default();
        let mut second = TargetWrites::default();
        let mut reserved = 0;
        for slot in 0..130 {
            first
                .begin(slot, |bytes| {
                    reserved += bytes;
                    Ok(())
                })
                .unwrap();
            second.begin(slot, |_| Ok(())).unwrap();
        }
        assert!(reserved <= 64);
        for slot in (0..130).rev() {
            assert!(first.pending(slot));
            assert!(first.pending(slot));
            assert!(first.take(slot));
            assert!(!first.pending(slot));
            assert!(!first.take(slot));
            assert!(second.take(slot));
            first
                .begin(slot, |_| panic!("same-frame reuse retains the bitset"))
                .unwrap();
            assert!(!second.take(slot));
            assert!(first.take(slot));
        }
    }
}

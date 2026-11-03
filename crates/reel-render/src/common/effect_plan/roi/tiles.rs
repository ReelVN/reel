//! Bounded tile demand on the unchanged shader grid. This is a pure candidate
//! plan: no replay path changes until its coverage and submission cost are admitted.
use super::{EffectSourceFootprint, Scissor, initialization, required_edges};

#[cfg(test)]
mod tests;

/// GXM region clips select 32-pixel tiles on surfaces at most 4096 pixels wide.
pub(crate) const EDGE: u32 = 32;
const ROWS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Span {
    left: u16,
    right: u16,
}
impl Span {
    fn empty(self) -> bool {
        self.left == self.right
    }
}

/// One conservative horizontal interval per tile row. Disjoint inputs may fill
/// gaps within a row; this can add work but never loses required texels. The
/// original pixel bounds still clip every emitted rectangle, including partial
/// surface edges. Each tile and each original output pixel is visited at most once.
#[derive(Clone, Debug)]
pub(crate) struct Cover {
    extent: [u32; 2],
    bounds: Scissor,
    rows: [Span; ROWS],
}

impl Cover {
    fn empty(bounds: Scissor, extent: [u32; 2]) -> Option<Self> {
        if extent.into_iter().any(|n| n == 0 || n > EDGE * ROWS as u32) {
            return None;
        }
        bounds.x.checked_add(bounds.width)?;
        bounds.y.checked_add(bounds.height)?;
        Some(Self {
            extent,
            bounds: bounds.intersect(Scissor::full(extent)),
            rows: [Span::default(); ROWS],
        })
    }

    /// Start from the existing draw scissor, retaining its exact pixel boundary.
    /// Unsupported dimensions leave the caller on its established rectangle path.
    pub(crate) fn rectangle(bounds: Scissor, extent: [u32; 2]) -> Option<Self> {
        let mut cover = Self::empty(bounds, extent)?;
        cover.insert(cover.bounds);
        Some(cover)
    }

    fn insert(&mut self, rectangle: Scissor) {
        let rectangle = rectangle.intersect(self.bounds);
        if rectangle.empty() {
            return;
        }
        let left = (rectangle.x / EDGE) as u16;
        let right = (rectangle.x + rectangle.width).div_ceil(EDGE) as u16;
        let first = (rectangle.y / EDGE) as usize;
        let end = (rectangle.y + rectangle.height).div_ceil(EDGE) as usize;
        for row in &mut self.rows[first..end] {
            *row = if row.empty() {
                Span { left, right }
            } else {
                Span {
                    left: row.left.min(left),
                    right: row.right.max(right),
                }
            };
        }
    }

    /// Region-clip tiles referenced by the planned draws, not shaded fragments.
    /// Tile execution also includes unchanged initialization and other scene draws.
    pub(crate) fn tile_count(&self) -> u32 {
        self.rows
            .iter()
            .map(|row| u32::from(row.right - row.left))
            .sum()
    }

    pub(crate) fn draw_count(&self) -> usize {
        self.rectangles().count()
    }

    pub(crate) fn bounds(&self) -> Scissor {
        self.bounds
    }

    /// Merge adjacent rows with identical intervals. Internal Y boundaries are
    /// tile aligned, so two draws never reference the same tile. Exact fragment
    /// clipping at the unchanged outer bounds remains the backend's responsibility.
    pub(crate) fn rectangles(&self) -> impl Iterator<Item = Scissor> + '_ {
        let mut y = 0;
        std::iter::from_fn(move || {
            while y < ROWS && self.rows[y].empty() {
                y += 1;
            }
            if y == ROWS {
                return None;
            }
            let first = y;
            let span = self.rows[y];
            y += 1;
            while y < ROWS && self.rows[y] == span {
                y += 1;
            }
            Some(
                Scissor {
                    x: u32::from(span.left) * EDGE,
                    y: first as u32 * EDGE,
                    width: u32::from(span.right - span.left) * EDGE,
                    height: (y - first) as u32 * EDGE,
                }
                .intersect(self.bounds),
            )
        })
    }

    fn tiles(&self) -> impl Iterator<Item = Scissor> + '_ {
        self.rows.iter().enumerate().flat_map(move |(y, span)| {
            (span.left..span.right).map(move |x| {
                Scissor {
                    x: u32::from(x) * EDGE,
                    y: y as u32 * EDGE,
                    width: EDGE,
                    height: EDGE,
                }
                .intersect(self.bounds)
            })
        })
    }

    #[cfg(test)]
    fn contains(&self, x: u32, y: u32) -> bool {
        if x >= self.extent[0]
            || y >= self.extent[1]
            || x < self.bounds.x
            || x >= self.bounds.x + self.bounds.width
            || y < self.bounds.y
            || y >= self.bounds.y + self.bounds.height
        {
            return false;
        }
        let span = self.rows[(y / EDGE) as usize];
        (u32::from(span.left)..u32::from(span.right)).contains(&(x / EDGE))
    }
}

pub(crate) struct Backward {
    pub cover: Cover,
    /// Calls to the existing f32 ABI enclosure, for CPU planning cost accounting.
    pub queries: u32,
}

/// Carry actual consumer demand through one effect's source reads. Coordinates
/// are the already-uploaded f32 ABI, including crop/storage maps. Source bounds
/// are the producer's existing draw scissor; initialization outside that scissor
/// stays unchanged. A repeat-addressed neighbor beyond a physical edge conservatively
/// spans the whole axis before intersection with those original producer bounds.
pub(crate) fn backward(
    context: &[[f32; 4]; crate::effect::CONTEXT_SLOTS],
    footprint: EffectSourceFootprint,
    consumer: &Cover,
    source_extent: [u32; 2],
    source_bounds: Scissor,
) -> Option<Backward> {
    let mut cover = Cover::empty(source_bounds, source_extent)?;
    if context[5][2..] != source_extent.map(|n| n as f32) {
        return None;
    }
    let mut queries = 0;
    let mut add = |rectangle| -> Option<()> {
        let edges = required_edges(context, footprint, rectangle, source_extent)?;
        if let Some(required) = initialization(edges, source_extent) {
            cover.insert(required);
        }
        queries += 1;
        Some(())
    };
    // Axis-wise maps commute with rectangular unions, including their clamps.
    // Merged rectangles avoid per-tile proof work on the common same-grid chain.
    // A rotated/sheared map keeps individual consumer tiles: mapping a whole
    // horizontal row to an AABB would discard much of its geometric advantage.
    let diagonal =
        context[0][1] == 0. && context[1][0] == 0. && context[2][1] == 0. && context[3][0] == 0.;
    if diagonal {
        for rectangle in consumer.rectangles() {
            add(rectangle)?;
        }
    } else {
        for tile in consumer.tiles() {
            add(tile)?;
        }
    }
    Some(Backward { cover, queries })
}

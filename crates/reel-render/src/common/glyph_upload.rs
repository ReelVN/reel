//! Bounded glyph uploads. Only exact rectangular unions may share a write;
//! atlas pixels between unrelated allocations always keep their previous value.
use super::{GpuError, glyph::GlyphBitmap};
use reel_core::budget::{BudgetPool, BudgetTicket};

const MAX_PATCHES: usize = 64;
const MAX_ALPHA_BYTES: usize = 64 * 1024;

struct Patch<T> {
    owner: T,
    origin: [u32; 2],
    bitmap: GlyphBitmap,
}

pub(crate) struct Batch<T> {
    patches: Vec<Patch<T>>,
    bytes: usize,
    metadata: Option<BudgetTicket>,
}
impl<T> Default for Batch<T> {
    fn default() -> Self {
        Self {
            patches: Vec::new(),
            bytes: 0,
            metadata: None,
        }
    }
}
#[derive(Clone, Copy, Default)]
struct Group {
    origin: [u32; 2],
    size: [u32; 2],
    first: usize,
}

pub(crate) struct Upload<'a, T> {
    pub owner: &'a T,
    pub origin: [u32; 2],
    pub size: [u32; 2],
    group: usize,
    membership: &'a [usize; MAX_PATCHES],
    patches: &'a [Patch<T>],
}
impl<T> Upload<'_, T> {
    /// Write alpha into an already-zeroed destination, including each glyph's
    /// transparent border. Storage may be CPU memory or a write-only GPU map.
    pub fn write_rows(&self, stride: usize, mut write: impl FnMut(usize, &[u8])) {
        for (index, patch) in self.patches.iter().enumerate() {
            if self.membership[index] != self.group {
                continue;
            }
            let offset = (patch.origin[1] - self.origin[1]) as usize * stride
                + (patch.origin[0] - self.origin[0]) as usize;
            patch
                .bitmap
                .padded_rows(stride, |at, data| write(offset + at, data));
        }
    }
}
impl<T> Batch<T> {
    pub fn full(&self, bitmap: &GlyphBitmap) -> bool {
        self.patches.len() == MAX_PATCHES
            || !self.patches.is_empty() && self.bytes + bitmap.alpha.len() > MAX_ALPHA_BYTES
    }

    pub fn push(
        &mut self,
        owner: T,
        origin: [u32; 2],
        bitmap: GlyphBitmap,
        pool: &BudgetPool,
        limit: u64,
    ) -> Result<(), GpuError> {
        debug_assert!(!self.full(&bitmap));
        if self.patches.len() == self.patches.capacity() {
            let capacity = (self.patches.capacity().max(4) * 2).min(MAX_PATCHES);
            let bytes = (capacity * size_of::<Patch<T>>()) as u64;
            let ticket = pool.reserve(bytes).map_err(|_| GpuError::Pressure {
                domain: "glyph batch",
                requested: bytes,
                available: limit.saturating_sub(pool.usage().bytes),
            })?;
            let mut patches = Vec::with_capacity(capacity);
            patches.append(&mut self.patches);
            self.patches = patches;
            self.metadata = Some(ticket);
        }
        self.bytes += bitmap.alpha.len();
        self.patches.push(Patch {
            owner,
            origin,
            bitmap,
        });
        Ok(())
    }

    pub fn flush(
        &mut self,
        same_page: impl Fn(&T, &T) -> bool,
        mut upload: impl FnMut(Upload<'_, T>) -> Result<(), GpuError>,
        mut accepted: impl FnMut(&T),
    ) -> Result<(), GpuError> {
        let mut groups = [Group::default(); MAX_PATCHES];
        let mut membership = [0; MAX_PATCHES];
        let count = self.patches.len();
        for (index, patch) in self.patches.iter().enumerate() {
            groups[index] = Group {
                origin: patch.origin,
                size: [patch.bitmap.size[0] + 2, patch.bitmap.size[1] + 2],
                first: index,
            };
            membership[index] = index;
        }
        // Atlas allocations are disjoint. Equal union area proves that merging
        // writes no intervening texels belonging to another live glyph.
        for index in 0..count {
            loop {
                let mut merged = false;
                for prior in 0..index {
                    if membership[prior] != prior
                        || !same_page(&self.patches[prior].owner, &self.patches[index].owner)
                    {
                        continue;
                    }
                    if let Some(joined) = join(groups[prior], groups[index]) {
                        groups[index] = joined;
                        for group in &mut membership[..index] {
                            if *group == prior {
                                *group = index;
                            }
                        }
                        merged = true;
                    }
                }
                if !merged {
                    break;
                }
            }
        }
        for index in 0..count {
            if membership[index] != index {
                continue;
            }
            let group = groups[index];
            upload(Upload {
                owner: &self.patches[group.first].owner,
                origin: group.origin,
                size: group.size,
                group: index,
                membership: &membership,
                patches: &self.patches,
            })?;
        }
        for patch in &self.patches {
            accepted(&patch.owner);
        }
        self.patches.clear();
        self.bytes = 0;
        Ok(())
    }
}

fn join(a: Group, b: Group) -> Option<Group> {
    let origin = [a.origin[0].min(b.origin[0]), a.origin[1].min(b.origin[1])];
    let size = [
        (a.origin[0] + a.size[0]).max(b.origin[0] + b.size[0]) - origin[0],
        (a.origin[1] + a.size[1]).max(b.origin[1] + b.size[1]) - origin[1],
    ];
    let area = |size: [u32; 2]| u64::from(size[0]) * u64::from(size[1]);
    (area(size) == area(a.size) + area(b.size)).then_some(Group {
        origin,
        size,
        first: a.first,
    })
}

#[cfg(test)]
mod tests;

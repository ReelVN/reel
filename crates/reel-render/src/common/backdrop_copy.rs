//! Integer-grid background snapshots shared by texture-transfer backends.
use super::scissor::Scissor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub source_origin: [u32; 2],
    pub destination_origin: [u32; 2],
    pub extent: [u32; 2],
}

impl Region {
    /// Both surfaces use the supplied sample count. Callers select matching
    /// copy-compatible formats and retain the original background read order.
    pub fn new(
        source_storage: [u32; 2],
        destination_storage: [u32; 2],
        source: Scissor,
        destination: Scissor,
        samples: u32,
    ) -> Option<Self> {
        if samples != 1
            || source.empty()
            || source.width != destination.width
            || source.height != destination.height
            || !contains(source_storage, source)
            || !contains(destination_storage, destination)
        {
            return None;
        }
        Some(Self {
            source_origin: [source.x, source.y],
            destination_origin: [destination.x, destination.y],
            extent: [source.width, source.height],
        })
    }

    /// A partial first write still needs initialized padding and unused pixels.
    pub fn overwrites_destination(self, storage: [u32; 2]) -> bool {
        self.destination_origin == [0; 2] && self.extent == storage
    }
}

fn contains(storage: [u32; 2], rect: Scissor) -> bool {
    rect.x
        .checked_add(rect.width)
        .is_some_and(|x| x <= storage[0])
        && rect
            .y
            .checked_add(rect.height)
            .is_some_and(|y| y <= storage[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::source_view::SourceView;
    use reel_ui::kurbo::Affine;

    #[test]
    fn placed_snapshot_copies_parent_pixels_into_its_assigned_subregion() {
        let view = SourceView {
            storage_size: [32, 32],
            source_rect: Scissor {
                x: 0,
                y: 0,
                width: 17,
                height: 13,
            },
            parent_rect: Scissor {
                x: 23,
                y: 41,
                width: 17,
                height: 13,
            },
            parent_to_source: Affine::translate((-23., -41.)),
        }
        .placed([128, 64], [64, 16]);
        let region = Region::new(
            [1024, 576],
            view.storage_size,
            view.parent_rect,
            view.source_rect,
            1,
        )
        .unwrap();
        assert_eq!(
            region,
            Region {
                source_origin: [23, 41],
                destination_origin: [64, 16],
                extent: [17, 13],
            }
        );
        let origin = view.origin();
        for (axis, offset) in origin.into_iter().enumerate() {
            for edge in [0, region.extent[axis] - 1] {
                let parent = region.source_origin[axis] + edge;
                let copied = region.destination_origin[axis] + edge;
                assert_eq!(parent as i64 - i64::from(offset), i64::from(copied));
            }
        }
        assert!(!region.overwrites_destination(view.storage_size));
    }

    #[test]
    fn copies_preserve_sample_count_bounds_and_full_initialization() {
        let full = Scissor::full([65, 11]);
        let copy = Region::new([65, 11], [65, 11], full, full, 1).unwrap();
        assert!(copy.overwrites_destination([65, 11]));
        assert!(!copy.overwrites_destination([96, 32]));
        assert!(Region::new([65, 11], [65, 11], full, full, 4).is_none());
        assert!(Region::new([64, 11], [65, 11], full, full, 1).is_none());
        assert!(Region::new([65, 11], [65, 10], full, full, 1).is_none());
        assert!(Region::new([65, 11], [65, 11], full, Scissor { width: 64, ..full }, 1).is_none());
    }
}

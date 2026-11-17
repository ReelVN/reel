//! Borrow a complete ordered image source; save its charged records on admission.
use super::*;
use crate::common::{scissor::Scissor, scopes};

#[derive(Clone)]
pub(super) enum Images<'a> {
    Borrowed(Recipe<'a>),
    Stored(Arc<Stored>),
}
#[derive(Clone)]
pub(super) struct Recipe<'a> {
    drawing: &'a FrameDrawing,
    commands: &'a [FrameCommand],
    owner: u64,
    outer: Affine,
    size: [u32; 2],
    uncropped: bool,
    count: usize,
    bounds: Scissor,
}
pub(super) struct Stored {
    owner: u64,
    records: Vec<Recording>,
    bounds: Scissor,
    _storage: BudgetTicket,
}

impl Images<'_> {
    pub(super) fn owner(&self) -> u64 {
        match self {
            Self::Borrowed(r) => r.owner,
            Self::Stored(r) => r.owner,
        }
    }
    fn bounds(&self) -> Scissor {
        match self {
            Self::Borrowed(r) => r.bounds,
            Self::Stored(r) => r.bounds,
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Borrowed(r) => r.count,
            Self::Stored(r) => r.records.len(),
        }
    }
    fn records(&self) -> impl Iterator<Item = Option<Recording>> {
        let (recipe, stored) = match self {
            Self::Borrowed(r) => (Some(r), &[][..]),
            Self::Stored(r) => (None, r.records.as_slice()),
        };
        recipe
            .into_iter()
            .flat_map(|r| {
                r.commands
                    .iter()
                    .filter(|c| matches!(c.op, FrameOp::Draw(_)))
                    .map(|c| r.record(c))
            })
            .chain(stored.iter().cloned().map(Some))
    }
    pub(super) fn same(&self, other: &Images<'_>) -> bool {
        self.owner() == other.owner()
            && self.len() == other.len()
            && self.bounds() == other.bounds()
            && self
                .records()
                .zip(other.records())
                .all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => {
                        a.local == b.local && Arc::ptr_eq(&a.scene, &b.scene) && a.same_pixels(&b)
                    }
                    _ => false,
                })
    }
    pub(super) fn into_owned(self, pool: &BudgetPool) -> Option<Images<'static>> {
        if let Self::Stored(stored) = self {
            return Some(Images::Stored(stored));
        }
        let bytes = size_of::<Stored>()
            .checked_add(2 * size_of::<usize>())?
            .checked_add(self.len().checked_mul(size_of::<Recording>())?)?;
        let mut storage = pool.reserve(bytes as u64).ok()?;
        let mut records = Vec::new();
        records.try_reserve_exact(self.len()).ok()?;
        storage
            .resize(
                (size_of::<Stored>()
                    + 2 * size_of::<usize>()
                    + records.capacity() * size_of::<Recording>()) as u64,
            )
            .ok()?;
        for record in self.records() {
            records.push(record?);
        }
        Some(Images::Stored(Arc::new(Stored {
            owner: self.owner(),
            bounds: self.bounds(),
            records,
            _storage: storage,
        })))
    }
}
impl Recipe<'_> {
    fn record(&self, command: &FrameCommand) -> Option<Recording> {
        let FrameOp::Draw(
            draw @ FrameDraw::Fill {
                transform,
                brush_transform,
                brush: Brush::Image(brush),
                ..
            },
        ) = &command.op
        else {
            return None;
        };
        let (recording, local) = self.drawing.recording(command.origin)?;
        if !recording.mask_path().is_empty() {
            return None;
        }
        Some(Recording {
            owner: self.owner,
            scene: recording.root().upgrade()?,
            local,
            image: ImageProjection::new(brush, *transform, *brush_transform, self.outer),
            geometry: self
                .uncropped
                .then(|| geometry::image(self.drawing, draw, self.outer, self.size))
                .flatten(),
            bounds: self.bounds,
        })
    }
}

pub(super) fn probe<'a>(
    drawing: &'a FrameDrawing,
    range: Range<usize>,
    effect: &'a FrameEffect,
    mut grid: Grid,
    uncropped: bool,
) -> Option<Probe<'a>> {
    let commands = drawing.commands().get(range)?;
    let FrameOp::PushGroup(group) = &commands.first()?.op else {
        return None;
    };
    if group.mask.is_some() || !matches!(commands.last()?.op, FrameOp::PopGroup) {
        return None;
    }
    let outer = Affine::new(grid.source_transform.map(f64::from_bits));
    let mut recipe = Recipe {
        drawing,
        commands: &commands[1..commands.len() - 1],
        owner: group.owner.to_raw(),
        outer,
        size: grid.source_size,
        uncropped,
        count: 0,
        bounds: Scissor::full(grid.source_size),
    };
    let mut depth = 1;
    let mut closing = false;
    for command in recipe.commands {
        match &command.op {
            FrameOp::PushContext(_) | FrameOp::PopContext => {}
            FrameOp::PushGroup(group) if recipe.count == 0 && scopes::source_over(group, false) => {
                depth += 1;
                if let Some(bounds) = Scissor::group_bounds(group, outer, grid.source_size, false) {
                    recipe.bounds = recipe.bounds.intersect(bounds);
                }
            }
            FrameOp::PopGroup if recipe.count > 0 && depth > 1 => {
                depth -= 1;
                closing = true;
            }
            FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(_),
                composite,
                ..
            }) if !closing && *composite == Default::default() => {
                recipe.record(command)?;
                recipe.count += 1;
            }
            _ => return None,
        }
    }
    if depth != 1 || recipe.count < 2 {
        return None;
    }
    let [x, y, width, height] = grid.source_scissor;
    let region = Scissor {
        x,
        y,
        width,
        height,
    }
    .intersect(recipe.bounds);
    grid.source_scissor = [region.x, region.y, region.width, region.height];
    grid.source_transform = affine(Affine::IDENTITY);
    Some(Probe {
        source: Source::Multiple(Images::Borrowed(recipe)),
        effect,
        grid,
        required: None,
    })
}

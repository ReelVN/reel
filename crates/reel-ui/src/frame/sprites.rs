//! Ordered sprite payloads shared by recording and portable frames.
use super::*;

/// One centered sprite in batch-local coordinates. Size is its logical width;
/// the shared source rectangle supplies its aspect ratio. Rotation is radians.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpriteInstance {
    pub position: [f64; 2],
    pub size: f64,
    pub rotation: f64,
    pub alpha: f32,
}
impl SpriteInstance {
    pub fn is_valid(self) -> bool {
        self.position
            .into_iter()
            .chain([self.size, self.rotation])
            .all(f64::is_finite)
            && self.size >= 0.
            && self.alpha.is_finite()
            && (0. ..=1.).contains(&self.alpha)
    }
    /// Compile the rotating scale once while admitting a frame payload.
    pub fn compiled(self, bounds: Rect) -> FrameSprite {
        let (sin, cos) = self.rotation.sin_cos();
        let scale = self.size / bounds.width();
        FrameSprite {
            position: self.position,
            axis: [cos * scale, sin * scale],
            alpha: self.alpha,
        }
    }
}
/// A compiled centered sprite. The first axis includes its uniform scale and
/// rotation; the second axis is its perpendicular. No trigonometry is repeated
/// by geometry consumers. Each backend uses this same 40-byte CPU record.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrameSprite {
    pub position: [f64; 2],
    pub axis: [f64; 2],
    pub alpha: f32,
}
impl FrameSprite {
    pub fn is_valid(self) -> bool {
        self.position
            .into_iter()
            .chain(self.axis)
            .all(f64::is_finite)
            && self.alpha.is_finite()
            && (0. ..=1.).contains(&self.alpha)
    }
    pub fn has_area(self) -> bool {
        self.axis != [0.; 2]
    }
    /// Map shared source pixels to the recorded center and basis.
    pub fn transform(self, bounds: Rect) -> Affine {
        let [a, b] = self.axis;
        let center = bounds.center();
        Affine::new([
            a,
            b,
            -b,
            a,
            self.position[0] - a * center.x + b * center.y,
            self.position[1] - b * center.x - a * center.y,
        ])
    }
}

#[derive(Debug)]
struct Storage {
    values: Vec<SpriteInstance>,
    _ticket: BudgetTicket,
}
/// Immutable recording payload. Clones share one capacity reservation; a frame
/// builder accounts its own budgeted copy separately.
#[derive(Clone, Debug, Default)]
pub struct SpriteInstances(Option<Arc<Storage>>);
impl SpriteInstances {
    pub fn as_slice(&self) -> &[SpriteInstance] {
        self.0.as_ref().map_or(&[], |s| s.values.as_slice())
    }
    /// Record an ordered producer under its CPU-output budget. Allocation failure
    /// is propagated to frame admission by `PaintCtx::draw_resource_sprites`.
    pub fn try_collect(
        pool: &BudgetPool,
        sample: impl FnOnce(&mut dyn FnMut(SpriteInstance)),
    ) -> Result<Self, BuildErrorKind> {
        let overhead = size_of::<Storage>() + 2 * size_of::<usize>();
        let mut ticket: Option<BudgetTicket> = None;
        let mut values = Vec::new();
        let mut failed = false;
        sample(&mut |sprite| {
            if failed {
                return;
            }
            if values.len() == values.capacity() {
                let capacity = values.capacity().saturating_mul(2).max(16);
                let Some(bytes) = capacity
                    .checked_add(values.capacity())
                    .and_then(|n| n.checked_mul(size_of::<SpriteInstance>()))
                    .and_then(|n| n.checked_add(overhead))
                else {
                    failed = true;
                    return;
                };
                let mut next = Vec::new();
                let admission = if let Some(ticket) = ticket.as_mut() {
                    ticket.resize(bytes as u64)
                } else {
                    pool.reserve(bytes as u64).map(|value| ticket = Some(value))
                };
                if admission.is_err() || next.try_reserve_exact(capacity).is_err() {
                    failed = true;
                    return;
                }
                next.extend_from_slice(&values);
                values = next;
                if ticket
                    .as_mut()
                    .unwrap()
                    .resize((values.capacity() * size_of::<SpriteInstance>() + overhead) as u64)
                    .is_err()
                {
                    failed = true;
                    return;
                }
            }
            values.push(sprite);
        });
        if failed {
            return Err(BuildErrorKind::Bytes);
        }
        let Some(ticket) = ticket else {
            return Ok(Self::default());
        };
        Ok(Self(Some(Arc::new(Storage {
            values,
            _ticket: ticket,
        }))))
    }
}
/// One ordered image batch. Shared state is stored once; instances retain their
/// painter order through clipping, composition and backend mesh expansion.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameSprites {
    pub transform: Affine,
    pub bounds: Rect,
    pub brush: ImageBrush<FrameImage>,
    #[serde(with = "wire::CompositeDef")]
    pub composite: Composite,
    pub instances: Vec<FrameSprite>,
}
impl FrameSprites {
    /// Conservative local ink, computed from the same transforms used for drawing.
    pub fn ink_bounds(&self) -> InkBounds {
        self.instances
            .iter()
            .filter(|s| s.alpha > 0. && s.has_area())
            .fold(InkBounds::Empty, |ink, s| {
                ink.union(InkBounds::Rect(self.bounds).transformed(s.transform(self.bounds)))
            })
    }
}

impl Building {
    fn sprite_source(&self) -> Result<Option<&SpriteInstances>, BuildError> {
        let c = self.cursors.last().unwrap();
        if c.stream.is_some() {
            return Ok(None);
        }
        self.source.plan.layers[c.layer]
            .command_owners()
            .get(c.owner_range)
            .and_then(|range| range.sprites.as_ref())
            .map(|result| result.as_ref().map_err(|kind| self.error(*kind)))
            .transpose()
    }
    pub(super) fn sprite_bytes(&self) -> Result<usize, BuildError> {
        self.sprite_source()?.map_or(Ok(0), |source| {
            source
                .as_slice()
                .len()
                .checked_mul(size_of::<FrameSprite>())
                .ok_or_else(|| self.error(BuildErrorKind::Bytes))
        })
    }
    pub(super) fn sprite_op(&self, op: FrameOp) -> Result<FrameOp, BuildError> {
        let Some(source) = self.sprite_source()? else {
            return Ok(op);
        };
        let FrameOp::Draw(FrameDraw::Fill {
            transform,
            shape: Geometry::Rect(bounds),
            brush: Brush::Image(brush),
            composite,
            ..
        }) = op
        else {
            return Err(self.error(BuildErrorKind::InvalidRecord));
        };
        if bounds.width() <= 0. || bounds.height() <= 0. {
            return Err(self.error(BuildErrorKind::InvalidRecord));
        }
        Ok(FrameOp::Draw(FrameDraw::Sprites(FrameSprites {
            transform,
            bounds,
            brush,
            composite,
            instances: Vec::with_capacity(source.as_slice().len()),
        })))
    }
    pub(super) fn copy_sprites(
        &mut self,
        p: &mut BuildProgress,
        budget: &StepBudget,
    ) -> Result<bool, BuildError> {
        let start = self.job.as_ref().unwrap().index;
        let source = self.sprite_source()?.expect("sprite source").as_slice();
        let remaining = source.len() - start;
        let units = 1 + 2 * self.cursors.last().unwrap().path.len() as u64;
        let bytes = size_of::<FrameSprite>() as u64;
        let count = (remaining as u64)
            .min((budget.max_units - p.units) / units)
            .min((budget.max_bytes - p.bytes) / bytes) as usize;
        if remaining != 0 && count == 0 {
            p.minimum_units = units;
            p.minimum_bytes = bytes;
            return Ok(false);
        }
        let source = self.sprite_source()?.expect("sprite source").clone();
        let job = self.job.as_mut().unwrap();
        let FrameOp::Draw(FrameDraw::Sprites(batch)) = &mut job.command.op else {
            unreachable!()
        };
        for &instance in &source.as_slice()[start..start + count] {
            if !instance.is_valid() {
                return Err(BuildError {
                    at: job.command.origin.at,
                    kind: BuildErrorKind::InvalidRecord,
                });
            }
            let instance = instance.compiled(batch.bounds);
            if !instance.is_valid() {
                return Err(BuildError {
                    at: job.command.origin.at,
                    kind: BuildErrorKind::InvalidRecord,
                });
            }
            if instance.alpha > 0. && instance.has_area() {
                job.ink = job.ink.union(
                    InkBounds::Rect(batch.bounds).transformed(instance.transform(batch.bounds)),
                );
            }
            batch.instances.push(instance);
        }
        job.index += count;
        p.units += count as u64 * units;
        p.bytes += count as u64 * bytes;
        if count == remaining {
            job.field = 9;
        }
        Ok(true)
    }
}

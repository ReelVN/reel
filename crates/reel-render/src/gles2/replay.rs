use super::*;
use super::{programs::Values, resources::Target};
use crate::common::{
    renderer::{Budgets, Reservation, reserve},
    scissor::Scissor,
    scopes::{Kind, Scopes, push_stack},
};
use reel_ui::{
    frame::{FrameCommand, FrameDrawing, FrameOp, RecordLocation, SceneTransition},
    imaging::{Composite, MaskMode, record::Clip},
    kurbo::Affine,
};
#[derive(Clone, Copy)]
pub(super) enum CompositeOperation {
    SourceOver,
    AlphaMask,
    LuminanceMask,
    Transition(f32),
    MaskedTransition(f32),
}
pub(super) struct Context<'a> {
    pub capture: bool,
    pub gpu: &'a native::Context,
    pub programs: &'a programs::Programs,
    pub limits: crate::common::GpuLimits,
    pub size: [u32; 2],
    pub budgets: &'a Budgets,
    pub budget_scope: Budgets,
    pub tickets: Vec<Reservation>,
    pub images: HashMap<ResourceHandle, Rc<Image>>,
    pub image_cache: &'a mut Images,
    pub glyph_cache: &'a mut draw::GlyphCache,
    pub glyphs: Vec<Rc<draw::Glyph>>,
    pub targets: Vec<Target>,
    pub available_targets: Vec<Target>,
    pub buffers: Vec<(native::VertexBuffer, native::IndexBuffer)>,
    pub scissor: Scissor,
    pub at: Option<RecordLocation>,
}
pub(super) struct Work {
    _tickets: Vec<Reservation>,
    _images: HashMap<ResourceHandle, Rc<Image>>,
    _glyphs: Vec<Rc<draw::Glyph>>,
    _targets: Vec<Target>,
    _buffers: Vec<(native::VertexBuffer, native::IndexBuffer)>,
}
pub(super) struct Prepared {
    pub output: Target,
    _work: Work,
}
pub(super) fn prepare(
    gpu: &native::Context,
    programs: &programs::Programs,
    budgets: &Budgets,
    limits: crate::common::GpuLimits,
    image_cache: &mut Images,
    glyph_cache: &mut draw::GlyphCache,
    images: HashMap<ResourceHandle, Rc<Image>>,
    frame: &RenderFrame,
    size: [u32; 2],
    capture: bool,
) -> Result<Prepared, (GlesError, Work)> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Replay);
    let mut context = Context {
        capture,
        gpu,
        programs,
        limits,
        size,
        budgets,
        budget_scope: Budgets::new(limits),
        tickets: Vec::new(),
        images,
        image_cache,
        glyph_cache,
        glyphs: Vec::new(),
        targets: Vec::new(),
        available_targets: Vec::new(),
        buffers: Vec::new(),
        scissor: Scissor::full(size),
        at: None,
    };
    let transform = Affine::scale_non_uniform(
        f64::from(size[0]) / frame.info().logical_size[0],
        f64::from(size[1]) / frame.info().logical_size[1],
    );
    let output = context.stream(frame.drawing(), transform);
    context.glyphs.extend(context.glyph_cache.take_uploads());
    let work = Work {
        _tickets: context.tickets,
        _images: context.images,
        _glyphs: context.glyphs,
        _targets: context.targets,
        _buffers: context.buffers,
    };
    match output {
        Ok(output) => Ok(Prepared {
            output,
            _work: work,
        }),
        Err(error) => Err((error, work)),
    }
}

struct Scope<'a> {
    target: Target,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    opacity: f32,
    invert: f32,
    blend: reel_ui::frame::BlendMode,
    scissor: Scissor,
    isolated: bool,
}
struct MaskComposition<'a> {
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    source: Target,
    opacity: f32,
    invert: f32,
    blend: reel_ui::frame::BlendMode,
    operation: CompositeOperation,
}
enum Pending<'a> {
    Mask(MaskComposition<'a>),
    Transition {
        definition: &'a SceneTransition,
        previous: Option<Target>,
    },
}
struct Stream<'a> {
    drawing: &'a FrameDrawing,
    // Masks have their own command space; frame exclusions apply to drawing ranges.
    offset: Option<usize>,
    transition_next: usize,
    transitions_enabled: bool,
    commands: &'a [FrameCommand],
    next: usize,
    transform: Affine,
    scopes: Scopes<Scope<'a>>,
    pending: Option<Pending<'a>>,
}
impl Context<'_> {
    pub fn target(&mut self) -> Result<Target, GlesError> {
        let target = if let Some(target) = self.available_targets.pop() {
            target
        } else {
            let bytes = u64::from(self.size[0]) * u64::from(self.size[1]) * 4;
            let local =
                self.budget_scope
                    .targets
                    .reserve(bytes)
                    .map_err(|_| GlesError::Budget {
                        domain: "offscreen targets",
                        requested: bytes,
                        available: self
                            .limits
                            .target_bytes
                            .saturating_sub(self.budget_scope.targets.usage().bytes),
                    })?;
            let target = match Target::new(
                self.gpu,
                self.size,
                &self.budgets.targets,
                self.limits.target_bytes,
            ) {
                Ok(target) => target,
                Err((error, storage)) => {
                    self.tickets.push(Reservation {
                        _shared: storage,
                        _local: local,
                    });
                    return Err(error);
                }
            };
            self.tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            self.targets.push(target.clone());
            target
        };
        self.gpu
            .clear(native::Target::Framebuffer(&target.framebuffer), [0.; 4])?;
        Ok(target)
    }
    fn recycle_target(&mut self, target: Target) -> Result<(), GlesError> {
        self.available_targets.push(target);
        Ok(())
    }
    pub(super) fn stream(
        &mut self,
        frame: &FrameDrawing,
        transform: Affine,
    ) -> Result<Target, GlesError> {
        let first = self.start_stream(
            frame,
            frame.commands(),
            Some(0),
            true,
            transform,
            Scissor::full(self.size),
        )?;
        let mut streams = Vec::new();
        push_stack(&mut streams, first, |bytes| self.transient(bytes))?;
        loop {
            let current = streams.last_mut().expect("active replay stream");
            // A transition boundary is a gap, including an empty hidden subtree.
            if current.transitions_enabled
                && let Some(transition) = current.drawing.transitions().get(current.transition_next)
                && transition.commands.start == current.offset.unwrap() + current.next
            {
                current.transition_next += 1;
                current.next = transition.commands.end - current.offset.unwrap();
                if self.capture && !transition.capture_included {
                    continue;
                }
                self.scissor = current.scopes.last().scissor;
                let child = self.start_stream(
                    transition.previous.drawing(),
                    transition.previous.drawing().commands(),
                    Some(0),
                    true,
                    current.transform
                        * transition.parent_to_window
                        * transition.previous.to_parent(),
                    self.scissor,
                )?;
                self.combine(
                    &child.scopes.last().target,
                    &current.scopes.last().target.view,
                    &current.scopes.last().target.view,
                    1.,
                    0.,
                    CompositeOperation::Transition(0.),
                )?;
                current.pending = Some(Pending::Transition {
                    definition: transition,
                    previous: None,
                });
                push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                continue;
            }
            if self.capture
                && current.next < current.commands.len()
                && let Some(offset) = current.offset
                && let Some(end) = current.drawing.capture_exclusion_end(offset + current.next)
            {
                current.next = (end - offset).min(current.commands.len());
                current.transition_next = current
                    .drawing
                    .transitions()
                    .partition_point(|transition| transition.commands.start < end);
                continue;
            }
            let Some(command) = current.commands.get(current.next) else {
                let finished = streams.pop().unwrap().scopes.finish()?.target;
                let Some(parent) = streams.last_mut() else {
                    return Ok(finished);
                };
                self.scissor = parent.scopes.last().scissor;
                match parent.pending.take().expect("suspended composition") {
                    Pending::Mask(mask) => {
                        self.combine_group(
                            &parent.scopes.last().target,
                            &mask.source.view,
                            &finished.view,
                            reel_ui::frame::GroupComposite {
                                alpha: mask.opacity,
                                blend: mask.blend,
                            },
                            mask.invert,
                            mask.operation,
                            mask.image_mask,
                        )?;
                        self.recycle_target(mask.source)?;
                        self.recycle_target(finished)?;
                    }
                    Pending::Transition {
                        definition,
                        previous: None,
                    } => {
                        let child = self.start_stream(
                            parent.drawing,
                            &parent.drawing.commands()[definition.commands.clone()],
                            Some(definition.commands.start),
                            false,
                            parent.transform,
                            self.scissor,
                        )?;
                        self.combine(
                            &child.scopes.last().target,
                            &parent.scopes.last().target.view,
                            &parent.scopes.last().target.view,
                            1.,
                            0.,
                            CompositeOperation::Transition(0.),
                        )?;
                        parent.pending = Some(Pending::Transition {
                            definition,
                            previous: Some(finished),
                        });
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    }
                    Pending::Transition {
                        definition,
                        previous: Some(previous),
                    } => {
                        self.combine_transition(
                            &parent.scopes.last().target,
                            &previous.view,
                            &finished.view,
                            definition,
                            parent.transform,
                        )?;
                        self.recycle_target(previous)?;
                        self.recycle_target(finished)?;
                    }
                }
                continue;
            };
            current.next += 1;
            let at = command.origin.at;
            self.at = Some(at);
            let transform = current.transform;
            self.scissor = current.scopes.last().scissor;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    let scissor = Scissor::from_clip(clip, transform, self.size);
                    current.scopes.push(
                        Kind::Clip,
                        at,
                        Scope {
                            target: if scissor.is_some() {
                                current.scopes.last().target.clone()
                            } else {
                                self.target()?
                            },
                            clip: if scissor.is_some() { None } else { Some(clip) },
                            mask: None,
                            image_mask: None,
                            opacity: 1.,
                            invert: 0.,
                            blend: Default::default(),
                            scissor: scissor.map_or(self.scissor, |s| self.scissor.intersect(s)),
                            isolated: scissor.is_none(),
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    blend_code(group.composite.blend).ok_or(GlesError::Unsupported {
                        at,
                        record: "group blend mode",
                    })?;
                    if !group.filters.is_empty() {
                        return Err(GlesError::Unsupported {
                            at,
                            record: "group filters",
                        });
                    }
                    current.scopes.push(
                        Kind::Group,
                        at,
                        Scope {
                            target: self.target()?,
                            clip: group.clip.as_ref(),
                            mask: group.mask,
                            image_mask: group
                                .image_mask
                                .as_ref()
                                .map(|mask| (mask, transform * group.effect_to_target)),
                            opacity: group.composite.alpha,
                            invert: group.invert,
                            blend: group.composite.blend,
                            scissor: self.scissor,
                            isolated: true,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PopClip | FrameOp::PopGroup => {
                    let kind = if matches!(command.op, FrameOp::PopClip) {
                        Kind::Clip
                    } else {
                        Kind::Group
                    };
                    let entry = current.scopes.pop(kind, at)?;
                    let group = entry.value;
                    self.scissor = current.scopes.last().scissor;
                    if !group.isolated {
                        continue;
                    }
                    let mut source = group.target;
                    if let Some(clip) = group.clip {
                        let coverage = self.target()?;
                        self.clip(&coverage, clip, transform, entry.at)?;
                        if group.mask.is_none() {
                            // Fold coverage and opacity into the final source-over pass.
                            // No intermediate full-size clipped texture is needed.
                            self.combine_group(
                                &current.scopes.last().target,
                                &source.view,
                                &coverage.view,
                                reel_ui::frame::GroupComposite {
                                    alpha: group.opacity,
                                    blend: group.blend,
                                },
                                group.invert,
                                CompositeOperation::AlphaMask,
                                group.image_mask,
                            )?;
                            self.recycle_target(source)?;
                            self.recycle_target(coverage)?;
                            continue;
                        }
                        let clipped = self.target()?;
                        self.combine(
                            &clipped,
                            &source.view,
                            &coverage.view,
                            1.,
                            0.,
                            CompositeOperation::AlphaMask,
                        )?;
                        self.recycle_target(source)?;
                        self.recycle_target(coverage)?;
                        source = clipped;
                    }
                    if let Some(mask) = group.mask {
                        let definition = current.drawing.masks().get(&mask.mask).ok_or(
                            GlesError::Unsupported {
                                at,
                                record: "missing mask",
                            },
                        )?;
                        current.pending = Some(Pending::Mask(MaskComposition {
                            image_mask: group.image_mask,
                            source,
                            opacity: group.opacity,
                            invert: group.invert,
                            blend: group.blend,
                            operation: match definition.mode {
                                MaskMode::Alpha => CompositeOperation::AlphaMask,
                                MaskMode::Luminance => CompositeOperation::LuminanceMask,
                            },
                        }));
                        let child = self.start_stream(
                            current.drawing,
                            &definition.commands,
                            None,
                            false,
                            transform * mask.transform,
                            self.scissor,
                        )?;
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    } else {
                        self.combine_group(
                            &current.scopes.last().target,
                            &source.view,
                            &source.view,
                            reel_ui::frame::GroupComposite {
                                alpha: group.opacity,
                                blend: group.blend,
                            },
                            group.invert,
                            CompositeOperation::SourceOver,
                            group.image_mask,
                        )?;
                        self.recycle_target(source)?;
                    }
                }
                FrameOp::Draw(draw) => {
                    let mut batch_end = if current.transitions_enabled {
                        current
                            .drawing
                            .transitions()
                            .get(current.transition_next)
                            .map_or(current.commands.len(), |t| {
                                t.commands
                                    .start
                                    .saturating_sub(current.offset.unwrap())
                                    .min(current.commands.len())
                            })
                    } else {
                        current.commands.len()
                    };
                    if self.capture
                        && let Some(offset) = current.offset
                    {
                        let exclusions = current.drawing.capture_exclusions();
                        let next =
                            exclusions.partition_point(|range| range.end < offset + current.next);
                        if let Some(range) = exclusions.get(next) {
                            batch_end = batch_end.min(range.start.saturating_sub(offset));
                        }
                    }
                    let consumed = self.image_batch(
                        &current.scopes.last().target,
                        &current.commands[current.next - 1..batch_end],
                        transform,
                    )?;
                    if consumed == 0 {
                        self.draw(
                            current.drawing,
                            &current.scopes.last().target,
                            draw,
                            transform,
                            at,
                        )?;
                    } else {
                        current.next += consumed - 1;
                    }
                }
                FrameOp::External(_) => {
                    return Err(GlesError::Unsupported {
                        at,
                        record: "external producer",
                    });
                }
            }
        }
    }
    fn start_stream<'a>(
        &mut self,
        drawing: &'a FrameDrawing,
        commands: &'a [FrameCommand],
        offset: Option<usize>,
        transitions_enabled: bool,
        transform: Affine,
        scissor: Scissor,
    ) -> Result<Stream<'a>, GlesError> {
        Ok(Stream {
            drawing,
            offset,
            transitions_enabled,
            transition_next: drawing
                .transitions()
                .partition_point(|t| t.commands.start < offset.unwrap_or(0)),
            commands,
            next: 0,
            transform,
            scopes: Scopes::new(Scope {
                target: self.target()?,
                clip: None,
                mask: None,
                image_mask: None,
                opacity: 1.,
                invert: 0.,
                blend: Default::default(),
                scissor,
                isolated: true,
            }),
            pending: None,
        })
    }

    pub fn combine(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
    ) -> Result<(), GlesError> {
        self.composition(
            target, source, mask, source, alpha, invert, operation, 0, None,
        )
    }
    fn composition(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        backdrop: &native::Texture,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
        blend: u32,
        expression: Option<&Target>,
    ) -> Result<(), GlesError> {
        if self.scissor.empty() {
            return Ok(());
        }
        let (mode, progress) = match operation {
            CompositeOperation::SourceOver => (0., 0.),
            CompositeOperation::AlphaMask => (1., 0.),
            CompositeOperation::LuminanceMask => (2., 0.),
            CompositeOperation::Transition(p) => (3., p),
            CompositeOperation::MaskedTransition(p) => (4., p),
        };
        let shader = &self.programs.composite;
        let mut values = Values::new();
        for (name, unit) in [
            ("u_source", 0),
            ("u_mask", 1),
            ("u_backdrop", 2),
            ("u_expression", 3),
        ] {
            values.extend(shader.int(name, unit));
        }
        values.extend(shader.vec4("u_params", [alpha, mode, progress, 0.]));
        values.extend(shader.float("u_invert", invert));
        values.extend(shader.float("u_blend", blend as f32));
        values.extend(shader.float("u_has_expression", f32::from(expression.is_some())));
        let replacing = matches!(
            operation,
            CompositeOperation::Transition(_) | CompositeOperation::MaskedTransition(_)
        ) || blend != 0;
        self.programs.quad(
            self.gpu,
            native::Target::Framebuffer(&target.framebuffer),
            self.size,
            shader,
            &[
                source,
                mask,
                backdrop,
                expression.map_or(source, |t| &t.view),
            ],
            &values,
            if replacing {
                native::Blend::Replace
            } else {
                native::Blend::Premultiplied
            },
            self.gl_scissor(),
        )
    }
    fn combine_group(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        composite: reel_ui::frame::GroupComposite,
        invert: f32,
        operation: CompositeOperation,
        image_mask: Option<(&reel_ui::frame::FrameImageMask, Affine)>,
    ) -> Result<(), GlesError> {
        let blend = blend_code(composite.blend)
            .ok_or(GlesError::Device("unsupported group blend".into()))?;
        let expression = if let Some((definition, transform)) = image_mask {
            Some(self.mask_expression(definition, transform)?)
        } else {
            None
        };
        let backdrop = if blend != 0 {
            let backdrop = self.target()?;
            self.combine(
                &backdrop,
                &target.view,
                &target.view,
                1.,
                0.,
                CompositeOperation::SourceOver,
            )?;
            Some(backdrop)
        } else {
            None
        };
        self.composition(
            target,
            source,
            mask,
            backdrop.as_ref().map_or(source, |b| &b.view),
            composite.alpha,
            invert,
            operation,
            blend,
            expression.as_ref(),
        )?;
        if let Some(backdrop) = backdrop {
            self.recycle_target(backdrop)?;
        }
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    fn combine_transition(
        &mut self,
        target: &Target,
        old: &native::Texture,
        new: &native::Texture,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GlesError> {
        let expression = if let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        {
            Some(self.mask_expression(definition, outer * transition.mask_to_window)?)
        } else {
            None
        };
        let operation = if expression.is_some() {
            CompositeOperation::MaskedTransition(transition.progress)
        } else {
            CompositeOperation::Transition(transition.progress)
        };
        self.composition(
            target,
            old,
            new,
            old,
            1.,
            0.,
            operation,
            0,
            expression.as_ref(),
        )?;
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    pub fn transient(&mut self, bytes: u64) -> Result<(), GlesError> {
        self.tickets.push(reserve(
            &self.budgets.transient,
            &self.budget_scope.transient,
            bytes,
            self.limits.transient_bytes,
            "transient",
        )?);
        Ok(())
    }
    pub fn composite_supported(
        &self,
        composite: Composite,
        at: RecordLocation,
    ) -> Result<(), GlesError> {
        if composite.blend != Composite::default().blend {
            Err(GlesError::Unsupported {
                at,
                record: "blend mode",
            })
        } else {
            Ok(())
        }
    }
    pub fn gl_scissor(&self) -> Option<[u32; 4]> {
        Some([
            self.scissor.x,
            self.size[1] - self.scissor.y - self.scissor.height,
            self.scissor.width,
            self.scissor.height,
        ])
    }
}
pub(super) fn blend_code(blend: reel_ui::frame::BlendMode) -> Option<u32> {
    use reel_ui::frame::BlendMode;
    use reel_ui::peniko::{Compose, Mix};
    let BlendMode::Standard(blend) = blend else {
        return Some(4);
    };
    match (blend.mix, blend.compose) {
        (Mix::Normal, Compose::SrcOver) => Some(0),
        (Mix::Normal, Compose::PlusLighter) => Some(1),
        (Mix::Multiply, Compose::SrcOver) => Some(2),
        (Mix::Screen, Compose::SrcOver) => Some(3),
        _ => None,
    }
}

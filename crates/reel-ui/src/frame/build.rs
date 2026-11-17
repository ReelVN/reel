//! Budgeted frame construction and copying.
use super::*;

impl StepBudget {
    fn spend(&self, progress: &mut BuildProgress, units: u64, bytes: u64) -> bool {
        if units > self.max_units - progress.units || bytes > self.max_bytes - progress.bytes {
            return false;
        }
        progress.units += units;
        progress.bytes += bytes;
        true
    }
}

impl Building {
    pub(super) fn error(&self, kind: BuildErrorKind) -> BuildError {
        let c = &self.cursors[0];
        let c = self.cursors.last().unwrap_or(c);
        BuildError {
            kind,
            at: RecordLocation {
                fragment: c.layer,
                command: c.position,
                mask_depth: c.path.len(),
                owner: c.owner,
            },
        }
    }
    pub(super) fn charge(&mut self, bytes: usize) -> Result<(), BuildError> {
        let next = self
            .bytes
            .checked_add(bytes as u64)
            .filter(|n| *n <= self.limits.max_bytes)
            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        self.ticket
            .resize(next)
            .map_err(|_| self.error(BuildErrorKind::Bytes))?;
        self.bytes = next;
        Ok(())
    }
    pub(super) fn scene<'a>(source: &'a RecordedSource, c: &Cursor) -> Option<&'a Scene> {
        let VisualLayerKind::Scene(scene) = &source.plan.layers.get(c.layer)?.kind else {
            return None;
        };
        let mut scene = scene.as_ref();
        for &mask in &c.path {
            scene = &scene.mask(mask).scene;
        }
        Some(scene)
    }
    pub(super) fn program(&mut self, stream: Option<usize>) -> &mut Vec<FrameCommand> {
        match stream {
            None => &mut self.commands,
            Some(index) => &mut self.masks[index].commands,
        }
    }
    pub(super) fn step(&mut self, b: StepBudget) -> Result<BuildProgress, BuildError> {
        let mut p = BuildProgress::default();
        if self.source.key.is_cancelled() {
            return Err(self.error(BuildErrorKind::Cancelled));
        }
        while !self.complete {
            if b.max_commands == 0 {
                break;
            }
            if self.job.as_ref().is_some_and(|job| {
                job.field < COPY_FIELDS_END
                    && matches!(job.command.op, FrameOp::Draw(FrameDraw::Sprites(_)))
            }) {
                if !self.copy_sprites(&mut p, &b)? {
                    break;
                }
                continue;
            }
            if self.job.is_some() {
                // Payload elements advance in place; move the full command only at commit.
                if self.job.as_ref().unwrap().field < COPY_FIELDS_END {
                    let c = self.cursors.last().expect("active cursor");
                    let resolve_units = c.path.len() as u64 + 1;
                    if !b.spend(&mut p, resolve_units, 0) {
                        p.minimum_units = resolve_units;
                        break;
                    }
                    let scene = Self::scene(&self.source, c).expect("copy job scene");
                    let cmd = &scene.commands()[c.position];
                    let job = self.job.as_ref().unwrap();
                    // Glyph payloads are contiguous immutable input. Resolve the scene once
                    // per admitted batch while preserving the same per-element work/byte
                    // charges and continuation position used by one-element slices.
                    if job.field == 3
                        && let Some(run) = source_glyph(scene, cmd)
                        && job.index < run.glyphs.len()
                    {
                        let glyph_bytes = size_of::<record::Glyph>() as u64;
                        let count = (run.glyphs.len() - job.index) as u64;
                        let count = count
                            .min(1 + (b.max_units - p.units) / resolve_units)
                            .min((b.max_bytes - p.bytes) / glyph_bytes);
                        if count == 0 {
                            p.minimum_units = resolve_units;
                            p.minimum_bytes = glyph_bytes;
                            break;
                        }
                        let glyphs = &run.glyphs[job.index..job.index + count as usize];
                        if !glyphs.iter().copied().all(valid_glyph) {
                            return Err(self.error(BuildErrorKind::InvalidRecord));
                        }
                        let job = self.job.as_mut().unwrap();
                        let FrameOp::Draw(FrameDraw::GlyphRun(target)) = &mut job.command.op else {
                            unreachable!()
                        };
                        target.glyphs.extend_from_slice(glyphs);
                        job.index += count as usize;
                        p.units += (count - 1) * resolve_units;
                        p.bytes += count * glyph_bytes;
                        continue;
                    }
                    let size = field_size(scene, cmd, job.field, job.index);
                    if let Some(size) = size {
                        if !b.spend(&mut p, 0, size as u64) {
                            p.minimum_units = resolve_units;
                            p.minimum_bytes = size as u64;
                            break;
                        }
                        let job = self.job.as_mut().unwrap();
                        copy_field(scene, cmd, job)?;
                        job.index += 1;
                    } else {
                        let job = self.job.as_mut().unwrap();
                        job.field += 1;
                        job.index = 0;
                    }
                    continue;
                }
                let cursor = self.cursors.last().unwrap();
                let commit_units = cursor.scopes.len() as u64
                    + cursor.path.len() as u64
                    + if cursor.stream.is_none() { 3 } else { 1 };
                if !b.spend(&mut p, commit_units, 0) {
                    p.minimum_units = commit_units;
                    break;
                }
                let job = self.job.take().unwrap();
                self.commit(job)?;
                p.commands += 1;
                if p.commands >= b.max_commands {
                    break;
                }
                continue;
            }
            if p.commands >= b.max_commands || p.units >= b.max_units {
                break;
            }
            let c = self.cursors.last().unwrap();
            if c.layer >= self.source.plan.layers.len() {
                if !b.spend(&mut p, 1, 0) {
                    break;
                }
                let c = self.cursors.last().unwrap();
                if !c.scopes.is_empty() || !c.contexts.is_empty() {
                    return Err(self.error(BuildErrorKind::InvalidRecord));
                }
                self.complete = true;
                break;
            }
            // Prepay even the source-path probe, owner validation/group search and External
            // resource/ink lookup. Credit survives a slice too small for the lookup.
            let group_search = u64::from(
                usize::BITS
                    - self.source.plan.layers[c.layer]
                        .groups()
                        .len()
                        .leading_zeros(),
            );
            let owner_work = if c.stream.is_some() {
                0
            } else if matches!(
                self.source.plan.layers[c.layer].kind,
                VisualLayerKind::External { .. }
            ) {
                3
            } else {
                1
            };
            let mapping_work = 2 * self.mappings.len() as u64
                + 4 * c.path.len() as u64
                + 2 * self.resources.leases().len() as u64
                + c.scopes.len() as u64
                + group_search
                + owner_work
                + 8;
            let available = (mapping_work - self.start_credit).min(b.max_units - p.units);
            p.units += available;
            self.start_credit += available;
            if self.start_credit < mapping_work {
                p.minimum_units = 1;
                break;
            }
            let c = self.cursors.last().unwrap();
            if let Some(scene) = Self::scene(&self.source, c) {
                if c.position == scene.commands().len() {
                    let advance_units = if c.stream.is_none() { 3 } else { 1 };
                    if !b.spend(&mut p, advance_units, 0) {
                        p.minimum_units = advance_units;
                        break;
                    }
                    self.start_credit = 0;
                    let c = self.cursors.last().unwrap();
                    if c.stream.is_some() {
                        if !c.scopes.is_empty() || !c.contexts.is_empty() {
                            return Err(self.error(BuildErrorKind::InvalidRecord));
                        }
                        let cursor = self.cursors.pop().unwrap();
                        self.bytes -= cursor.storage_bytes() as u64;
                        drop(cursor);
                        self.ticket
                            .resize(self.bytes)
                            .expect("shrinking cursor storage");
                    } else {
                        self.cursors
                            .last_mut()
                            .unwrap()
                            .advance_fragment(&self.source.plan);
                    }
                    continue;
                }
            } else {
                if !b.spend(&mut p, 0, size_of::<FrameCommand>() as u64) {
                    p.minimum_bytes = size_of::<FrameCommand>() as u64;
                    break;
                }
                self.start_credit = 0;
                let c = self.cursors.last().unwrap();
                let layer = &self.source.plan.layers[c.layer];
                let VisualLayerKind::External {
                    token,
                    bounds,
                    content_transform,
                } = layer.kind
                else {
                    unreachable!()
                };
                let resource = self
                    .resources
                    .external(token)
                    .ok_or_else(|| self.error(BuildErrorKind::MissingExternal { token }))?;
                let transform = layer.transform * content_transform;
                if !finite(transform) || !bounds.is_finite() {
                    return Err(self.error(BuildErrorKind::InvalidRecord));
                }
                let command = FrameCommand {
                    origin: CommandOrigin {
                        source: c.layer as u32,
                        local: LocalId::None,
                        at: self.error(BuildErrorKind::Incomplete).at,
                    },
                    op: FrameOp::External(ExternalDraw {
                        resource,
                        token,
                        owner: layer.widget_id,
                        local_bounds: bounds,
                        content_to_window: transform,
                    }),
                };
                self.add_ink(inflate_aa(
                    InkBounds::Rect(bounds).transformed(transform),
                    1. / self.source.geometry.scale_factor(),
                ));
                self.commands.push(command);
                p.commands += 1;
                self.cursors
                    .last_mut()
                    .unwrap()
                    .advance_fragment(&self.source.plan);
                continue;
            }
            if self.allocation_need.is_none() {
                if !b.spend(&mut p, 0, size_of::<FrameCommand>() as u64) {
                    p.minimum_bytes = size_of::<FrameCommand>() as u64;
                    break;
                }
                // Validate the already selected range once after prepayment.
                // Owner was advanced by the prior commit, before any new spend.
                let c = self.cursors.last().unwrap();
                if c.stream.is_none()
                    && !self.source.plan.layers[c.layer].command_owners().is_empty()
                {
                    let layer = &self.source.plan.layers[c.layer];
                    layer
                        .command_owners()
                        .get(c.owner_range)
                        .filter(|range| {
                            range.commands.contains(&c.position) && range.owner == c.owner
                        })
                        .ok_or_else(|| self.error(BuildErrorKind::InvalidRecord))?;
                }
                self.allocation_need = Some(self.next_allocation()?);
            }
            let need = self.allocation_need.expect("prepared allocation");
            let bytes = (need - self.allocation_credit).min(b.max_bytes - p.bytes);
            p.bytes += bytes;
            self.allocation_credit += bytes;
            if self.allocation_credit < need {
                p.minimum_bytes = 1;
                break;
            }
            // Reservation credits may span slices; actual vector reservation is
            // atomic and measured separately from payload copies and source Drop.
            self.start_credit = 0;
            self.allocation_need = None;
            self.allocation_credit = 0;
            self.start(need)?;
        }
        p.complete = self.complete;
        Ok(p)
    }
    pub(super) fn next_allocation(&self) -> Result<u64, BuildError> {
        let cursor = self.cursors.last().unwrap();
        let scene = Self::scene(&self.source, cursor).unwrap();
        let command = &scene.commands()[cursor.position];
        let sprite_bytes = self.sprite_bytes()?;
        let mut bytes = payload_bytes(scene, command)
            .and_then(|n| n.checked_add(sprite_bytes))
            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        let stack_bytes = match command {
            Command::PushClip(_) => growth_bytes(&cursor.scopes)
                .and_then(|n| n.checked_add(growth_bytes(&cursor.clips)?)),
            Command::PushGroup(_) => growth_bytes(&cursor.scopes)
                .and_then(|n| n.checked_add(growth_bytes(&cursor.groups)?)),
            Command::PushContext(_) => growth_bytes(&cursor.contexts),
            _ => Some(0),
        };
        bytes = stack_bytes
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
        if let Command::PushGroup(id) = command
            && let Some(mask) = &scene.group(*id).mask
        {
            let scene_key = scene as *const Scene as usize;
            if !self
                .mappings
                .iter()
                .any(|m| m.scene == scene_key && m.local == mask.mask)
            {
                bytes = scene
                    .mask(mask.mask)
                    .scene
                    .commands()
                    .len()
                    .checked_mul(size_of::<FrameCommand>())
                    .and_then(|n| {
                        n.checked_add(
                            cursor
                                .path
                                .len()
                                .checked_add(1)?
                                .checked_mul(2 * size_of::<record::MaskId>())?,
                        )
                    })
                    .and_then(|n| n.checked_add(growth_bytes(&self.masks)?))
                    .and_then(|n| n.checked_add(growth_bytes(&self.mappings)?))
                    .and_then(|n| n.checked_add(growth_bytes(&self.cursors)?))
                    .and_then(|n| n.checked_add(growth_bytes(&self.recordings)?))
                    // Immutable mask Arc and conservative B-tree node storage at publication.
                    .and_then(|n| {
                        n.checked_add(size_of::<FrameMask>() + 2 * size_of::<usize>() + 1024)
                    })
                    .and_then(|n| n.checked_add(bytes))
                    .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
            }
        }
        if matches!(command, Command::PushGroup(_)) && cursor.stream.is_none() {
            let layer = &self.source.plan.layers[cursor.layer];
            if let Ok(index) = layer
                .groups()
                .binary_search_by_key(&cursor.position, |group| group.command)
            {
                let group = &layer.groups()[index];
                for effect in group.effect.iter().chain(group.mask_effect.iter()) {
                    bytes = bytes
                        .checked_add(std::mem::size_of_val(effect.instance.values()))
                        .and_then(|bytes| {
                            bytes.checked_add(
                                effect
                                    .images
                                    .len()
                                    .checked_mul(size_of::<ResourceHandle>())?,
                            )
                        })
                        .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
                }
                if let Some(mask) = &group.image_mask {
                    bytes = bytes
                        .checked_add(
                            mask.images
                                .len()
                                .checked_mul(size_of::<ResourceHandle>())
                                .ok_or_else(|| self.error(BuildErrorKind::Bytes))?,
                        )
                        .ok_or_else(|| self.error(BuildErrorKind::Bytes))?;
                }
            }
        }
        if (bytes as u64)
            .checked_add(self.bytes)
            .is_none_or(|n| n > self.limits.max_bytes)
        {
            return Err(self.error(BuildErrorKind::Bytes));
        }
        Ok(bytes as u64)
    }
    pub(super) fn start(&mut self, allocation: u64) -> Result<(), BuildError> {
        self.charge(allocation as usize)?;
        let allocation_error = self.error(BuildErrorKind::Bytes);
        let cursor = self.cursors.last().unwrap();
        let command = &Self::scene(&self.source, cursor).unwrap().commands()[cursor.position];
        let cursor = self.cursors.last_mut().unwrap();
        match command {
            Command::PushClip(_) => {
                reserve_slot(&mut cursor.scopes).map_err(|()| allocation_error)?;
                reserve_slot(&mut cursor.clips).map_err(|()| allocation_error)?;
            }
            Command::PushGroup(_) => {
                reserve_slot(&mut cursor.scopes).map_err(|()| allocation_error)?;
                reserve_slot(&mut cursor.groups).map_err(|()| allocation_error)?;
            }
            Command::PushContext(_) => {
                reserve_slot(&mut cursor.contexts).map_err(|()| allocation_error)?;
            }
            _ => {}
        }
        let c = self.cursors.last().unwrap();
        let layer = &self.source.plan.layers[c.layer];
        let scene = Self::scene(&self.source, c).unwrap();
        let command = &scene.commands()[c.position];
        let at = self.error(BuildErrorKind::Incomplete).at;
        let transform = if c.stream.is_some() {
            Affine::IDENTITY
        } else {
            layer.transform
        };
        if !finite(transform) {
            return Err(self.error(BuildErrorKind::InvalidRecord));
        }
        let source = if let Some(mask) = c.stream {
            self.source
                .plan
                .layers
                .len()
                .checked_add(mask)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| self.error(BuildErrorKind::Capacity))?
        } else {
            c.layer as u32
        };
        let mut descend = None;
        let mut mask = None;
        if let Command::PushGroup(id) = command
            && let Some(applied) = &scene.group(*id).mask
        {
            let scene_key = scene as *const Scene as usize;
            let found = self
                .mappings
                .iter()
                .find(|m| m.scene == scene_key && m.local == applied.mask)
                .map(|m| m.index);
            let index = found.unwrap_or(self.masks.len());
            let mask_id = u32::try_from(index).map_err(|_| self.error(BuildErrorKind::Capacity))?;
            if found.is_none() {
                let definition = scene.mask(applied.mask);
                let count = definition.scene.commands().len();
                let mode = definition.mode;
                let local = applied.mask;
                let t = transform * applied.transform;
                let VisualLayerKind::Scene(root) = &layer.kind else {
                    unreachable!()
                };
                let root = Arc::downgrade(root);
                let mut mask_path = Vec::with_capacity(c.path.len() + 1);
                mask_path.extend_from_slice(&c.path);
                mask_path.push(local);
                reserve_slot(&mut self.masks).map_err(|()| allocation_error)?;
                reserve_slot(&mut self.mappings).map_err(|()| allocation_error)?;
                reserve_slot(&mut self.cursors).map_err(|()| allocation_error)?;
                reserve_slot(&mut self.recordings).map_err(|()| allocation_error)?;
                self.recordings
                    .push(Some(RecordingSource { root, mask_path }));
                self.masks.push(FrameMask {
                    mode,
                    commands: Vec::with_capacity(count),
                });
                self.mappings.push(MaskMapping {
                    scene: scene_key,
                    local,
                    index,
                });
                descend = Some((local, index));
                mask = Some(MaskUse {
                    mask: mask_id,
                    transform: t,
                });
            } else {
                mask = Some(MaskUse {
                    mask: mask_id,
                    transform: transform * applied.transform,
                });
            }
        }
        let c = self.cursors.last().unwrap();
        let layer = &self.source.plan.layers[c.layer];
        let meta = if c.stream.is_some() {
            Some(GroupMetadata {
                command: c.position,
                owner: c.owner,
                effect_transform: Affine::IDENTITY,
                image_mask: None,
                effect: None,
                mask_effect: None,
                invert: 0.,
                blend: None,
            })
        } else {
            layer
                .groups()
                .binary_search_by_key(&c.position, |m| m.command)
                .ok()
                .map(|i| {
                    let mut metadata = layer.groups()[i].clone();
                    metadata.effect_transform = layer.transform * metadata.effect_transform;
                    metadata
                })
        };
        let scene = Self::scene(&self.source, self.cursors.last().unwrap()).unwrap();
        let command = &scene.commands()[self.cursors.last().unwrap().position];
        let image = if c.stream.is_none() {
            layer.command_owners().get(c.owner_range)
        } else {
            None
        };
        let image_paint = image.map_or_else(ImagePaint::default, |image| ImagePaint {
            color: image.image_color,
            appearance: image
                .image_appearance
                .map(|appearance| appearance.transformed(transform)),
            source_rect: image.image_source_rect,
            sampling_transform: image.image_sampling_transform,
            resource: image.image_resource,
        });
        let (local, op) = make_op(
            scene,
            command,
            transform,
            meta,
            mask,
            image_paint,
            &self.resources,
        )
        .map_err(|kind| self.error(kind))?;
        let op = self.sprite_op(op)?;
        validate_op(&op).map_err(|kind| self.error(kind))?;
        let ink = initial_ink(&op);
        self.job = Some(CopyJob {
            command: FrameCommand {
                origin: CommandOrigin { source, local, at },
                op,
            },
            // A zero allocation also proves there are no variable-size fields to copy.
            field: if allocation == 0 { COPY_FIELDS_END } else { 0 },
            index: 0,
            ink,
            descend,
        });
        Ok(())
    }
    pub(super) fn add_ink(&mut self, ink: InkBounds) {
        if let Some(group) = self
            .cursors
            .last_mut()
            .unwrap()
            .scopes
            .iter_mut()
            .rev()
            .find(|s| s.kind == ScopeKind::Group)
        {
            group.ink = group.ink.union(ink);
        }
    }
    pub(super) fn commit(&mut self, mut job: CopyJob) -> Result<(), BuildError> {
        let c = self.cursors.last().unwrap();
        let stream = c.stream;
        let index = match stream {
            None => self.commands.len(),
            Some(i) => self.masks[i].commands.len(),
        };
        let kind = match &job.command.op {
            FrameOp::PushClip(_) => Some(ScopeKind::Clip),
            FrameOp::PushGroup(_) => Some(ScopeKind::Group),
            _ => None,
        };
        if let Some(kind) = kind {
            let c = self.cursors.last_mut().unwrap();
            match kind {
                ScopeKind::Clip => c.clips.push(index),
                ScopeKind::Group => c.groups.push(index),
            }
            c.scopes.push(Scope {
                kind,
                command: index,
                ink: InkBounds::Empty,
            });
        } else if matches!(job.command.op, FrameOp::PopClip | FrameOp::PopGroup) {
            let expected = if matches!(job.command.op, FrameOp::PopClip) {
                ScopeKind::Clip
            } else {
                ScopeKind::Group
            };
            if c.scopes.last().is_none_or(|s| s.kind != expected) {
                let crossed = match expected {
                    ScopeKind::Clip => !c.clips.is_empty(),
                    ScopeKind::Group => !c.groups.is_empty(),
                };
                return Err(self.error(if crossed {
                    BuildErrorKind::UnsupportedRecord
                } else {
                    BuildErrorKind::InvalidRecord
                }));
            }
            let c = self.cursors.last_mut().unwrap();
            match expected {
                ScopeKind::Clip => {
                    c.clips.pop();
                }
                ScopeKind::Group => {
                    c.groups.pop();
                }
            }
            let scope = c.scopes.pop().unwrap();
            if scope.kind == ScopeKind::Group {
                let FrameOp::PushGroup(group) = &mut self.program(stream)[scope.command].op else {
                    unreachable!()
                };
                // Filters and shaped glyphs may extend arbitrarily beyond layout boxes.
                group.ink_bounds = if let Some(effect) = &group.effect {
                    InkBounds::Rect(effect.bounds).transformed(group.effect_to_target)
                } else if group.filters.is_empty() {
                    scope.ink
                } else {
                    InkBounds::Unbounded
                };
                let ink = group.ink_bounds;
                self.add_ink(ink);
            }
        } else if matches!(job.command.op, FrameOp::PushContext(_)) {
            self.cursors.last_mut().unwrap().contexts.push(index);
        } else if matches!(job.command.op, FrameOp::PopContext) {
            if self.cursors.last_mut().unwrap().contexts.pop().is_none() {
                return Err(self.error(BuildErrorKind::InvalidRecord));
            }
        } else if matches!(job.command.op, FrameOp::Draw(_)) {
            let ink = if stream.is_some() {
                // A reusable local mask can be sampled through different transforms.
                // Its target-space AA footprint is resolved by the backend per use.
                InkBounds::Unbounded
            } else {
                draw_ink(
                    &job.command.op,
                    job.ink,
                    1. / self.source.geometry.scale_factor(),
                )
            };
            self.add_ink(ink);
        }
        let owner = match &job.command.op {
            FrameOp::PushGroup(g) => g.owner,
            _ => job.command.origin.at.owner,
        };
        self.program(stream).push(job.command);
        self.cursors
            .last_mut()
            .unwrap()
            .advance_command(&self.source.plan);
        if let Some((mask, index)) = job.descend.take() {
            let parent = self.cursors.last().unwrap();
            let mut path = Vec::with_capacity(parent.path.len() + 1);
            path.extend_from_slice(&parent.path);
            path.push(mask);
            self.cursors.push(Cursor {
                layer: parent.layer,
                path,
                stream: Some(index),
                position: 0,
                scopes: Vec::new(),
                contexts: Vec::new(),
                clips: Vec::new(),
                groups: Vec::new(),
                owner,
                owner_range: 0,
            });
        }
        Ok(())
    }
}

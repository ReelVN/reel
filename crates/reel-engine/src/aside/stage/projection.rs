//! Reactive stage projection and native property synchronization.
use super::*;
mod index;
pub(super) use index::Index;

impl StageBinding {
    pub(in crate::aside) fn sync_tree(
        &mut self,
        heap: &mut Heap,
        root: NodeId,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        self.projection_index.invalid = true;
        let mut pending = vec![root];
        let mut nodes = Vec::new();
        while let Some(id) = pending.pop() {
            let node = self
                .scene
                .node(id)
                .ok_or(AdapterError::Input("retired stage node"))?;
            pending.extend(node.children.iter().copied());
            nodes.push(id);
        }
        self.sync_nodes(heap, nodes, read_limit)
    }

    fn sync_nodes(
        &mut self,
        heap: &mut Heap,
        nodes: impl IntoIterator<Item = NodeId>,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let mut updates = Vec::new();
        for id in nodes {
            #[cfg(test)]
            {
                self.projection_visits += 1;
            }
            let node = self
                .scene
                .node(id)
                .ok_or(AdapterError::Input("retired stage node"))?;
            let binding = self.handles.get_mut(&id).unwrap();
            let value = heap.import_handle(binding.handle)?;
            let fields = heap.host_fields(value)?;
            let version = heap.info(fields)?.content_version;
            let selected = if binding.overrides.is_some() {
                overrides::selection(heap, value)?
            } else {
                Value::NULL
            };
            if version == binding.fields_version
                && !binding.projection_dirty
                && binding.image_skin.is_none()
                && !matches!(node.kind, NodeKind::ScrollBar(_))
                && !super::super::effects::changed(heap, binding.effect_source)?
                && !mask_changed(heap, binding.mask_source)?
                && !binding
                    .overrides
                    .as_ref()
                    .map(|cache| cache.changed(heap, value))
                    .transpose()?
                    .unwrap_or(false)
            {
                continue;
            }
            let mut projected_image = None;
            let (mut kind, offset) = match &node.kind {
                _ if binding.shape == snapshot::Shape::PageStack => (
                    NodeKind::Stack,
                    super::super::pages::PageField::PROPERTIES.len(),
                ),
                _ if binding.shape == snapshot::Shape::TabBar => (
                    super::super::pages::tab_kind(heap, fields, read_limit)?,
                    super::super::pages::TabField::PROPERTIES.len(),
                ),
                NodeKind::Row(_) | NodeKind::Column(_) => (
                    flow::flow_kind(heap, fields, matches!(node.kind, NodeKind::Row(_)))?,
                    flow::FlowField::PROPERTIES.len(),
                ),
                NodeKind::Grid(_) => (
                    flow::grid_kind(heap, fields)?,
                    flow::GridField::PROPERTIES.len(),
                ),
                NodeKind::VirtualList(_) => (
                    NodeKind::VirtualList(super::super::lists::virtualized::project(heap, value)?),
                    0,
                ),
                NodeKind::Input { bindings, .. } => {
                    let state = binding.input.as_mut().unwrap();
                    let bindings = input::replace(heap, value, fields, bindings, state)?;
                    (
                        input::kind(heap, fields, bindings)?,
                        input::InputField::PROPERTIES.len(),
                    )
                }
                NodeKind::Camera(_) => (
                    camera::kind(heap, fields)?,
                    camera::CameraField::PROPERTIES.len(),
                ),
                NodeKind::Scroll { .. } => (
                    scroll::kind(heap, fields)?,
                    scroll::ScrollField::PROPERTIES.len(),
                ),
                NodeKind::ScrollBar(_) => (
                    scrollbar::kind(heap, fields)?,
                    scrollbar::ScrollBarField::PROPERTIES.len(),
                ),
                NodeKind::Popup(view) => (
                    popup::kind(heap, fields, view.hint)?,
                    binding.shape.prefix(),
                ),
                NodeKind::Slider { .. } => (
                    controls::slider_kind(heap, fields)?,
                    controls::SliderField::PROPERTIES.len(),
                ),
                NodeKind::ProgressBar { .. } => (
                    controls::progress_kind(heap, fields)?,
                    controls::ProgressField::PROPERTIES.len(),
                ),
                NodeKind::Toggle { appearance, .. } => (
                    controls::toggle_kind(heap, fields, read_limit, *appearance)?,
                    controls::ToggleField::PROPERTIES.len(),
                ),
                NodeKind::Radio { revision, .. } => {
                    let option = heap
                        .table_entry(fields, controls::RadioField::Option.index() as u32)?
                        .1;
                    let changed = !binding
                        .radio_option
                        .is_some_and(|old| old.same_bits(option));
                    (
                        controls::radio_kind(
                            heap,
                            fields,
                            revision.wrapping_add(u64::from(changed)),
                            read_limit,
                        )?,
                        controls::RadioField::PROPERTIES.len(),
                    )
                }
                NodeKind::Button { .. } => (
                    button_kind(
                        heap,
                        heap.table_entry(fields, 0)?.1,
                        heap.table_entry(fields, 1)?.1,
                        heap.table_entry(fields, 2)?.1,
                        heap.table_entry(fields, 3)?.1,
                        read_limit,
                    )?,
                    appearance::ButtonField::PROPERTIES.len(),
                ),
                NodeKind::Text(text) => {
                    let source = heap.table_entry(fields, 0)?.1;
                    (
                        NodeKind::Text(if binding.text_source == Some(heap.handle(source)?) {
                            text.clone()
                        } else {
                            super::super::rich_text::read(heap, source, read_limit)?
                        }),
                        2,
                    )
                }
                NodeKind::TextInput {
                    value,
                    multiline,
                    options,
                } => {
                    let source = heap.table_entry(fields, 0)?.1;
                    let projected = if binding.text_source == Some(heap.handle(source)?)
                        || binding.text_source.is_none()
                            && text_input::matches_text(heap, source, value)?
                    {
                        value.clone()
                    } else {
                        Arc::from(read_text(heap, source, read_limit)?)
                    };
                    if binding.text_history.as_ref().is_some_and(|h| {
                        h.state == text_input::HistoryState::Invalidated
                            || Some(h.expected) != heap.handle(source).ok()
                    }) || binding.text_source != Some(heap.handle(source)?)
                        && binding.text_history.is_none()
                    {
                        let host = heap.import_handle(binding.handle)?;
                        let text = &projected;
                        let mut selected = text_input::selection(heap, host)?;
                        let boundary = |mut offset: usize| {
                            offset = offset.min(text.len());
                            while !text.is_char_boundary(offset) {
                                offset -= 1;
                            }
                            offset
                        };
                        selected.anchor = boundary(selected.anchor);
                        selected.focus = boundary(selected.focus);
                        let revision = text_input::revision(heap, host)?
                            .checked_add(1)
                            .ok_or(HeapError::IdentityExhausted)?;
                        text_input::publish(heap, host, None, selected, false, false)?;
                        heap.set_host_payload(host, 4, Value::int(revision as i64))?;
                        binding.text_history = None;
                    } else if binding
                        .text_history
                        .as_ref()
                        .is_some_and(|h| h.state == text_input::HistoryState::ResetPublished)
                    {
                        binding.text_history = None;
                    }
                    (
                        NodeKind::TextInput {
                            multiline: *multiline,
                            options: text_input::options(heap, fields, Some(options), read_limit)?,
                            value: projected,
                        },
                        text_input::TextInputField::PROPERTIES.len(),
                    )
                }
                NodeKind::Particles(batch) => {
                    let mut batch = batch.clone();
                    batch.size_scale = nonnegative(heap.table_entry(fields, 2)?.1)?;
                    (
                        NodeKind::Particles(batch),
                        super::super::particles::PREFIX.len(),
                    )
                }
                NodeKind::Image { source, origin, .. } => {
                    let table = heap.table_read(fields)?;
                    let raw_source = table.value(0)?;
                    let raw_rect = table.value(11)?;
                    let skin = binding.shape == snapshot::Shape::Image
                        && image_skin::is_skin(heap, raw_source);
                    let (asset, rect) = if skin {
                        image_skin::select(heap, raw_source, raw_rect)?
                    } else {
                        (raw_source, raw_rect)
                    };
                    let asset_handle = heap.handle(asset)?;
                    projected_image = Some((
                        asset_handle,
                        skin.then(|| heap.handle(raw_source)).transpose()?,
                    ));
                    (
                        NodeKind::Image {
                            source: if binding.shape == snapshot::Shape::Video
                                || binding.text_source == Some(asset_handle)
                            {
                                source.clone()
                            } else {
                                read_source(heap, asset, read_limit)?
                            },
                            fit: read_fit(heap, table.value(1)?)?,
                            slice: read_slice(heap, table.value(2)?)?,
                            slice_scale: nonnegative(table.value(3)?)?,
                            repeat: read_image_repeat(heap, table.value(4)?, table.value(2)?)?,
                            tile_scale: positive(table.value(5)?)?,
                            corner_colors: read_corner_colors(heap, table.value(6)?)?,
                            slice_center: boolean(table.value(7)?)?,
                            adjustment: read_image_adjustment(heap, fields)?,
                            source_rect: read_source_rect(heap, rect)?,
                            alpha_layout: read_alpha_layout(heap, table.value(12)?)?,
                            sampling: read_sampling(heap, table.value(13)?)?,
                            origin: origin.clone(),
                        },
                        registry::IMAGE_PREFIX.len(),
                    )
                }
                kind => (kind.clone(), 0),
            };
            controls::project_group(heap, value, &mut kind)?;
            let mut props = read_props(
                heap,
                fields,
                offset,
                selected,
                read_limit,
                property_values::PropertyContext {
                    previous: Some(property_values::PreviousProperties {
                        mask_source: binding.mask_source,
                        effect_source: binding.effect_source,
                        props: &node.props,
                    }),
                    appearance_origin: node.props.appearance_origin.clone(),
                    effects: &self.effect_products,
                    effect_projection: &mut self.effect_projection,
                },
            )?;
            overrides::apply_offsets(heap, value, &mut props)?;
            super::super::pages::semantics(binding.shape, &kind, &mut props);
            validate_text_props(&kind, &props)?;
            if matches!(kind, NodeKind::VirtualList(_)) {
                validate_virtual_props(&props)?;
            }
            let mask_source = mask_handle(heap, fields, offset, selected)?;
            let effect_source = effect_stamp(heap, fields, offset, selected)?;
            let version = if matches!(kind, NodeKind::TextInput { .. }) {
                heap.info(fields)?.content_version
            } else {
                version
            };
            updates.push((
                id,
                kind,
                props,
                version,
                mask_source,
                effect_source,
                projected_image,
            ));
        }
        for (id, kind, props, version, mask_source, effect_source, projected_image) in updates {
            let mut kind = kind;
            let host = heap.import_handle(self.handles[&id].handle)?;
            text_input::project(heap, host, &mut kind)?;
            text_input::project_queue(self.handles[&id].text_history.as_deref(), &mut kind);
            if matches!(kind, NodeKind::Radio { .. }) {
                let value = heap.import_handle(self.handles[&id].handle)?;
                self.handles.get_mut(&id).unwrap().radio_option = Some(
                    heap.table_entry(
                        heap.host_fields(value)?,
                        controls::RadioField::Option.index() as u32,
                    )?
                    .1,
                );
            }
            if let Some((asset, skin)) = projected_image {
                let binding = self.handles.get_mut(&id).unwrap();
                binding.text_source = Some(asset);
                self.projection_index.invalid |= binding.image_skin != skin;
                binding.image_skin = skin;
            } else if matches!(kind, NodeKind::Text(_) | NodeKind::TextInput { .. }) {
                let value = heap.import_handle(self.handles[&id].handle)?;
                let source = heap.table_entry(heap.host_fields(value)?, 0)?.1;
                self.handles.get_mut(&id).unwrap().text_source = Some(heap.handle(source)?);
                if matches!(kind, NodeKind::Text(_)) {
                    self.handles.get_mut(&id).unwrap().text_listening =
                        heap.table_entry(heap.host_fields(value)?, 1)?.1.tag() != ValueTag::Null;
                }
            }
            self.scene.update(id, kind, props)?;
            let binding = self.handles.get_mut(&id).unwrap();
            self.projection_index.invalid |= binding.mask_source.map(|(handle, _)| handle)
                != mask_source.map(|(handle, _)| handle)
                || binding.effect_source.map(|(handle, _)| handle)
                    != effect_source.map(|(handle, _)| handle);
            binding.fields_version = version;
            binding.projection_dirty = false;
            if let Some(cache) = &mut binding.overrides {
                cache.accept(heap, heap.import_handle(binding.handle)?)?;
            }
            binding.mask_source = mask_source;
            binding.effect_source = effect_source;
            let fields = heap.host_fields(heap.import_handle(binding.handle)?)?;
            let selected = overrides::selection(heap, host)?;
            binding.interaction_listening =
                interaction::listening(heap, fields, binding.interaction_offset, selected)?;
        }
        Ok(())
    }

    pub fn sync(&mut self, heap: &mut Heap, read_limit: usize) -> Result<(), AdapterError> {
        self.effect_projection.collect();
        let stamp = |heap: &Heap, scene: &Stage| {
            (
                heap.id(),
                heap.epoch(),
                heap.mutation_revision(),
                scene.revision(),
            )
        };
        if self.last_sync == Some(stamp(heap, &self.scene)) {
            return Ok(());
        }
        #[cfg(test)]
        {
            self.sync_count += 1;
        }
        let incremental =
            self.last_sync.is_some() && self.projection_index.collect(heap, &self.scene)?;
        if incremental {
            let pending = std::mem::take(&mut self.projection_index.pending);
            let result = self.sync_nodes(heap, pending.iter().copied(), read_limit);
            self.projection_index.pending = pending;
            result?;
        } else if let Some(root) = self.scene.root() {
            self.sync_tree(heap, root, read_limit)?;
        }
        // Every node keeps its complete component alive. A collected detached
        // root therefore means the whole component has lost script ownership.
        let lost = self
            .scene
            .detached_roots()
            .filter(|id| heap.import_handle(self.handles[id].handle).is_err())
            .collect::<Vec<_>>();
        for root in lost {
            let retired = self.scene.discard(root)?;
            self.retire(heap, retired)?;
        }
        if self.projection_index.invalid || !incremental {
            self.projection_index
                .rebuild(heap, &self.scene, &self.handles)?;
        } else {
            self.projection_index.accept(heap);
        }
        self.last_sync = Some(stamp(heap, &self.scene));
        Ok(())
    }
}

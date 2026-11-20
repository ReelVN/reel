//! Scene control construction and incremental property updates.
use super::*;

impl SceneWidget {
    fn slider_visuals(view: &SceneNode, art: reel_ui::stage::SliderArt) -> ([bool; 2], f64) {
        let [track, thumb] = [art.track, art.thumb].map(|part| {
            part.is_some_and(|id| view.children.iter().any(|child| child.node.id == id))
        });
        let radius = if thumb {
            art.thumb_width * 0.5
        } else if view.node.props.decorated {
            10.
        } else {
            0.
        };
        ([!track, !thumb], radius)
    }

    pub(super) fn visual_caption(node: &SceneProperties) -> Option<&str> {
        if node.props.semantics == reel_ui::stage::NodeSemantics::Tab && !node.props.decorated {
            None
        } else {
            Self::caption(&node.kind)
        }
    }
    pub(super) fn image_widget(view: &SceneNode) -> Option<WidgetPod<Image>> {
        let NodeKind::Image {
            fit,
            slice,
            slice_scale,
            repeat,
            tile_scale,
            corner_colors,
            adjustment,
            slice_center,
            source_rect,
            alpha_layout,
            sampling,
            ..
        } = &view.node.kind
        else {
            return None;
        };
        view.images
            .image_resource
            .as_ref()
            .map(|lease| Image::from_resource(lease.clone()))
            .or_else(|| {
                view.images
                    .tiled_size
                    .map(|size| Image::tiled(size, view.images.tiles.clone()))
            })
            .map(|image| {
                NewWidget::new(
                    image
                        .with_source_rect(*source_rect)
                        .with_alpha_layout(*alpha_layout)
                        .with_sampling(*sampling)
                        .with_color(color(view.node.props.color))
                        .with_slice(*slice, *slice_scale, *slice_center)
                        .with_pattern(*repeat, *tile_scale, *corner_colors)
                        .with_color_adjustment(*adjustment),
                )
                .with_props(*fit)
                .to_pod()
            })
    }
    pub(super) fn content_changed(old: &NodeKind, new: &NodeKind) -> bool {
        match (old, new) {
            (NodeKind::ScrollBar(a), NodeKind::ScrollBar(b)) => !a.same_geometry(*b),
            (
                NodeKind::Image {
                    source: a,
                    source_rect: ar,
                    fit: af,
                    slice: aslice,
                    slice_scale: ascale,
                    repeat: arep,
                    tile_scale: atile,
                    corner_colors: acol,
                    adjustment: aadj,
                    alpha_layout: aalpha,
                    slice_center: acenter,
                    ..
                },
                NodeKind::Image {
                    source: b,
                    source_rect: br,
                    fit: bf,
                    slice: bslice,
                    slice_scale: bscale,
                    repeat: brep,
                    tile_scale: btile,
                    corner_colors: bcol,
                    adjustment: badj,
                    alpha_layout: balpha,
                    slice_center: bcenter,
                    ..
                },
            ) => {
                // A moving sampling window is paint feedback, not replacement
                // of a surrounding button's authored content or input owner.
                (
                    a,
                    ar.map(|r| [r.width() as f32, r.height() as f32]),
                    af,
                    aslice,
                    ascale,
                    arep,
                    atile,
                    acol,
                    aadj,
                    aalpha,
                    acenter,
                ) != (
                    b,
                    br.map(|r| [r.width() as f32, r.height() as f32]),
                    bf,
                    bslice,
                    bscale,
                    brep,
                    btile,
                    bcol,
                    badj,
                    balpha,
                    bcenter,
                )
            }
            _ => old != new,
        }
    }
    pub(super) fn caption(kind: &NodeKind) -> Option<&str> {
        match kind {
            NodeKind::Text(text) => Some(text),
            NodeKind::Button { label, .. }
            | NodeKind::Toggle { label, .. }
            | NodeKind::Radio { label, .. } => label.as_deref(),
            _ => None,
        }
    }
    pub(super) fn is_button(&self) -> bool {
        matches!(
            self.node.kind,
            NodeKind::Button { .. } | NodeKind::Toggle { .. } | NodeKind::Radio { .. }
        )
    }
    pub(super) fn reveal_fade(node: &SceneProperties) -> Option<reel_ui::core::RevealFade> {
        node.props
            .reveal_fade
            .map(|progress| reel_ui::core::RevealFade {
                from: node.props.reveal_fade_from,
                progress,
                step: node.props.reveal_fade_step,
            })
    }
    pub(super) fn reveal_range(node: &SceneProperties) -> std::ops::Range<usize> {
        let text = Self::visual_caption(node).unwrap_or("");
        let mut end = node.props.reveal.unwrap_or(text.len()).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        0..end
    }
    pub(super) fn new_pod(
        view: &Arc<SceneNode>,
        nodes: &mut BTreeMap<NodeId, WidgetId>,
    ) -> WidgetPod<Self> {
        let pod = WidgetPod::new(Self::new(view, nodes));
        nodes.insert(view.node.id, pod.id());
        pod
    }
    pub(super) fn forget(view: &Arc<SceneNode>, nodes: &mut BTreeMap<NodeId, WidgetId>) {
        nodes.remove(&view.node.id);
        for child in view.physical_children() {
            Self::forget(child, nodes);
        }
    }
    pub(super) fn new(view: &Arc<SceneNode>, nodes: &mut BTreeMap<NodeId, WidgetId>) -> Self {
        let label = match &view.node.kind {
            NodeKind::Text(_)
            | NodeKind::Button { .. }
            | NodeKind::Toggle { .. }
            | NodeKind::Radio { .. } => Some(
                NewWidget::new(
                    Label::new("")
                        .with_rich_text(match &view.node.kind {
                            NodeKind::Text(text) => text.clone(),
                            _ => Self::visual_caption(&view.node).unwrap_or("").into(),
                        })
                        .with_visible_bytes(Self::reveal_range(&view.node))
                        .with_reveal_fade(Self::reveal_fade(&view.node))
                        .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                        .with_style(StyleProperty::FontWeight(
                            reel_ui::reel_text::FontWeight::new(view.node.props.font_weight as f32),
                        ))
                        .with_style(StyleProperty::LetterSpacing(
                            view.node.props.letter_spacing as f32,
                        ))
                        .with_style(StyleProperty::WordSpacing(
                            view.node.props.word_spacing as f32,
                        ))
                        .with_writing_mode(view.node.props.writing_mode)
                        .with_text_alignment(view.node.props.text_align)
                        .with_text_fit(view.node.props.text_fit)
                        .with_outline(text_outline(&view.node.props))
                        .with_shadow(text_shadow(&view.node.props))
                        .with_gradient(view.node.props.text_gradient)
                        .with_style(StyleProperty::LineHeight(
                            reel_ui::reel_text::LineHeight::FontSizeRelative(
                                view.node.props.line_height as f32,
                            ),
                        ))
                        .with_style(font_style(&view.node.props.font)),
                )
                .with_props(text_line_breaking(&view.node))
                .with_props(ContentColor::new(color(view.node.props.color)))
                .to_pod(),
            ),
            _ => None,
        };
        Self {
            node: view.node.clone(),
            projection: view.clone(),
            sync_dirty: false,
            button: ButtonBehavior::default(),
            interaction: None,
            input: match &view.node.kind {
                NodeKind::Input {
                    bindings,
                    keyboard,
                    pointer,
                    wheel,
                    ..
                } => Some(NodeInput::new(
                    bindings.clone(),
                    *keyboard,
                    *pointer,
                    *wheel,
                )),
                _ => None,
            },
            label,
            editor: if let NodeKind::TextInput {
                value,
                multiline,
                options,
            } = &view.node.kind
            {
                let editor = TextInput::with_mode(value.clone(), *multiline)
                    .with_selection(options.selection, options.selection_revision)
                    .with_edit_filter(options.filter_edits)
                    .with_editing(options.pending_edit, options.replay.clone())
                    .with_read_only(options.read_only)
                    .with_password(options.password)
                    .with_selection_color(color(options.selection_color))
                    .with_caret_color(options.caret_color.map(color))
                    .with_outline(text_outline(&view.node.props))
                    .with_shadow(text_shadow(&view.node.props))
                    .with_text_alignment(view.node.props.text_align)
                    .with_text_wrap(view.node.props.text_wrap)
                    .with_style(StyleProperty::LineHeight(
                        reel_ui::reel_text::LineHeight::FontSizeRelative(
                            view.node.props.line_height as f32,
                        ),
                    ))
                    .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                    .with_style(StyleProperty::FontWeight(
                        reel_ui::reel_text::FontWeight::new(view.node.props.font_weight as f32),
                    ))
                    .with_style(StyleProperty::LetterSpacing(
                        view.node.props.letter_spacing as f32,
                    ))
                    .with_style(StyleProperty::WordSpacing(
                        view.node.props.word_spacing as f32,
                    ))
                    .with_style(font_style(&view.node.props.font));
                let editor = if let Some(hint) = &options.placeholder {
                    editor.with_placeholder(hint.clone())
                } else {
                    editor
                };
                Some(
                    NewWidget::new(editor)
                        .with_props(ContentColor::new(color(view.node.props.color)))
                        .to_pod(),
                )
            } else {
                None
            },
            slider: if let NodeKind::Slider {
                range, value, art, ..
            } = view.node.kind
            {
                let ([track, thumb], radius) = Self::slider_visuals(view, art);
                let mut slider = Slider::new(range.min(), range.max(), value)
                    .with_decorated(view.node.props.decorated)
                    .with_paint_parts(track, thumb);
                if let Some(step) = range.step() {
                    slider = slider.with_step(step);
                }
                let tint = color(view.node.props.color);
                Some(
                    NewWidget::new(slider)
                        .with_props(ThumbRadius(Length::px(radius)))
                        .with_props(TrackThickness(Length::px(4.)))
                        .with_props(ThumbColor(tint))
                        .with_props(TrackColor {
                            active: tint,
                            inactive: tint.with_alpha(tint.components[3] * 0.3),
                        })
                        .to_pod(),
                )
            } else {
                None
            },
            scrollbar: if let NodeKind::ScrollBar(config) = view.node.kind {
                Some(reel_ui::widgets::ScrollBarState::new(config))
            } else {
                None
            },
            particle_sampler: Default::default(),
            particle_image: matches!(view.node.kind, NodeKind::Particles(_))
                .then(|| view.images.image_resource.clone())
                .flatten(),
            mask_images: view.images.mask_images.clone(),
            effect: view.images.effect.clone(),
            mask_effect: view.images.mask_effect.clone(),
            image: Self::image_widget(view),
            image_size: view.image_size(),
            children: view
                .physical_children()
                .map(|child| (child.node.clone(), Self::new_pod(child, nodes)))
                .collect(),
            size: Size::ZERO,
            scroll: ScrollState::default(),
            scroll_laid_out: false,
            scroll_request: match view.node.kind {
                NodeKind::Scroll { offset, .. } if offset != Vec2::ZERO => Some(offset),
                _ => None,
            },
            scroll_feedback: None,
            scroll_input: false,
            scroll_baseline: match view.node.kind {
                NodeKind::Scroll { offset, .. } => offset,
                _ => Vec2::ZERO,
            },
            virtual_list: matches!(view.node.kind, NodeKind::VirtualList(_)).then(Default::default),
            virtual_viewport: None,
            #[cfg(test)]
            layouts: 0,
            #[cfg(test)]
            syncs: 0,
            #[cfg(test)]
            paint_work: [0; 3],
        }
    }
    pub(super) fn sync(
        this: &mut WidgetMut<'_, Self>,
        view: &Arc<SceneNode>,
        feedback: bool,
        nodes: &mut BTreeMap<NodeId, WidgetId>,
    ) -> bool {
        if !this.widget.sync_dirty && Arc::ptr_eq(&this.widget.projection, view) {
            return false;
        }
        #[cfg(test)]
        {
            this.widget.syncs += 1;
        }
        if this.widget.effect != view.images.effect
            || this.widget.mask_effect != view.images.mask_effect
        {
            this.widget.effect = view.images.effect.clone();
            this.widget.mask_effect = view.images.mask_effect.clone();
            this.ctx.request_pre_paint();
        }
        if !this
            .widget
            .mask_images
            .iter()
            .map(ResourceLease::handle)
            .eq(view.images.mask_images.iter().map(ResourceLease::handle))
        {
            this.widget.mask_images = view.images.mask_images.clone();
            this.ctx.request_pre_paint();
        }
        if matches!(view.node.kind, NodeKind::Particles(_)) {
            if this
                .widget
                .particle_image
                .as_ref()
                .and_then(|i| i.image_size())
                != view
                    .images
                    .image_resource
                    .as_ref()
                    .and_then(|i| i.image_size())
            {
                this.ctx.request_layout();
            }
            if this
                .widget
                .particle_image
                .as_ref()
                .map(ResourceLease::handle)
                != view
                    .images
                    .image_resource
                    .as_ref()
                    .map(ResourceLease::handle)
            {
                this.ctx.request_paint_only();
            }
            this.widget.particle_image = view.images.image_resource.clone();
        }
        if this.widget.node.props.capture_included != view.node.props.capture_included {
            this.ctx.request_pre_paint();
        }
        let children_changed = this.widget.sync_dirty
            || !Arc::ptr_eq(&this.widget.projection.children, &view.children);
        let order_changed = children_changed
            && !this
                .widget
                .children
                .iter()
                .map(|(node, _)| node.id)
                .eq(view.physical_children().map(|child| child.node.id));
        let mut content_changed = Self::content_changed(&this.widget.node.kind, &view.node.kind)
            || order_changed
            || this.widget.node.props.visible != view.node.props.visible
            || this.widget.node.props.enabled != view.node.props.enabled;
        if let (
            Some(pod),
            NodeKind::TextInput {
                value,
                multiline,
                options,
            },
        ) = (&mut this.widget.editor, &view.node.kind)
            && let Some(mut editor) = this.ctx.get_mut(pod)
        {
            TextInput::set_read_only(&mut editor, options.read_only);
            TextInput::set_password(&mut editor, options.password);
            TextInput::set_multiline(&mut editor, *multiline);
            TextInput::set_placeholder(&mut editor, options.placeholder.clone());
            TextInput::set_selection_color(&mut editor, color(options.selection_color));
            TextInput::set_caret_color(&mut editor, options.caret_color.map(color));
            TextInput::set_outline(&mut editor, text_outline(&view.node.props));
            TextInput::set_shadow(&mut editor, text_shadow(&view.node.props));
            TextInput::set_value(&mut editor, value.clone());
            TextInput::set_selection(&mut editor, options.selection, options.selection_revision);
            TextInput::set_text_alignment(&mut editor, view.node.props.text_align);
            TextInput::set_text_wrap(&mut editor, view.node.props.text_wrap);
            if this.widget.node.props.line_height != view.node.props.line_height {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::LineHeight(reel_ui::reel_text::LineHeight::FontSizeRelative(
                        view.node.props.line_height as f32,
                    )),
                );
            }
            if this.widget.node.props.font_size != view.node.props.font_size {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font_weight != view.node.props.font_weight {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::FontWeight(reel_ui::reel_text::FontWeight::new(
                        view.node.props.font_weight as f32,
                    )),
                );
            }
            if this.widget.node.props.letter_spacing != view.node.props.letter_spacing {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::LetterSpacing(view.node.props.letter_spacing as f32),
                );
            }
            if this.widget.node.props.word_spacing != view.node.props.word_spacing {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::WordSpacing(view.node.props.word_spacing as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                TextInput::insert_style(&mut editor, font_style(&view.node.props.font));
            }
            if this.widget.node.props.color != view.node.props.color {
                editor.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
            TextInput::set_edit_filter(&mut editor, options.filter_edits);
            TextInput::set_editing(&mut editor, options.pending_edit, options.replay.as_deref());
        }
        let mut move_slider_thumb = false;
        if let (
            Some(pod),
            NodeKind::Slider {
                range, value, art, ..
            },
        ) = (&mut this.widget.slider, &view.node.kind)
            && let Some(mut slider) = this.ctx.get_mut(pod)
        {
            Slider::configure(&mut slider, *range, *value);
            Slider::set_decorated(&mut slider, view.node.props.decorated);
            let (parts, radius) = Self::slider_visuals(view, *art);
            if let NodeKind::Slider {
                range: old_range,
                value: old_value,
                art: old_art,
                ..
            } = this.widget.node.kind
            {
                let (old_parts, old_radius) =
                    Self::slider_visuals(&this.widget.projection, old_art);
                if parts != old_parts {
                    Slider::set_paint_parts(&mut slider, parts[0], parts[1]);
                }
                if radius != old_radius {
                    slider.insert_prop(ThumbRadius(Length::px(radius)));
                }
                move_slider_thumb =
                    art.thumb.is_some() && (*range != old_range || *value != old_value);
            }
            if this.widget.node.props.color != view.node.props.color {
                let tint = color(view.node.props.color);
                slider.insert_prop(ThumbColor(tint));
                slider.insert_prop(TrackColor {
                    active: tint,
                    inactive: tint.with_alpha(tint.components[3] * 0.3),
                });
            }
        }
        if move_slider_thumb {
            this.ctx.request_compose();
        }
        if let (Some(state), NodeKind::ScrollBar(config)) =
            (&mut this.widget.scrollbar, &view.node.kind)
        {
            let previous = state.view();
            if state.configure(*config) {
                if previous.same_controls(*config) {
                    this.ctx.refresh_captured_input_configuration();
                } else {
                    this.ctx.invalidate_input_configuration();
                }
                this.ctx.request_layout();
            } else if previous != *config {
                this.ctx.request_compose();
            }
        }
        if let (
            Some(input),
            NodeKind::Input {
                bindings,
                keyboard,
                pointer,
                wheel,
                ..
            },
        ) = (&mut this.widget.input, &view.node.kind)
            && (input.bindings != *bindings
                || input.keyboard != *keyboard
                || input.pointer_enabled != *pointer
                || input.wheel_enabled != *wheel)
        {
            input.cancel(this.widget.node.id, |event| {
                this.ctx.submit_action::<NodeEvent>(event)
            });
            *input = NodeInput::new(bindings.clone(), *keyboard, *pointer, *wheel);
            this.ctx.set_interactive(
                this.widget.accepts_pointer_interaction(),
                this.widget.accepts_focus(),
            );
        }
        if this.widget.node.props.enabled != view.node.props.enabled {
            this.ctx.set_disabled(!view.node.props.enabled);
        }
        if this.widget.node.props.modal != view.node.props.modal {
            this.ctx
                .set_modal(view.node.props.modal && !matches!(view.node.kind, NodeKind::Popup(_)));
        }
        let has_image = matches!(view.node.kind, NodeKind::Image { .. })
            && (view.images.image_resource.is_some() || view.images.tiled_size.is_some());
        if has_image && this.widget.image.is_none() {
            this.widget.image = Self::image_widget(view);
            this.widget.image_size = view.image_size();
            this.ctx.children_changed();
        } else if !matches!(view.node.kind, NodeKind::Image { .. })
            && let Some(image) = this.widget.image.take()
        {
            this.ctx.remove_child(image);
            this.widget.image_size = Size::ZERO;
            this.ctx.children_changed();
        } else if !has_image
            && this.widget.projection.images != view.images
            && let Some(pod) = &mut this.widget.image
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            // Keep the mounted widget while releasing pixels from hidden views.
            // The image service can still evict idle resources under pressure.
            Image::set_tiles(&mut image, view.image_size(), Vec::new());
            image.ctx.request_render();
            this.widget.image_size = view.image_size();
        }
        if this.widget.node.props.color != view.node.props.color
            && let Some(pod) = &mut this.widget.image
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_color(&mut image, color(view.node.props.color));
        }
        let image_source_changed = matches!(
            (&this.widget.node.kind, &view.node.kind),
            (NodeKind::Image { source: old, .. }, NodeKind::Image { source, .. }) if old != source
        );
        if (image_source_changed
            || this
                .widget
                .projection
                .images
                .image_resource
                .as_ref()
                .map(ResourceLease::handle)
                != view
                    .images
                    .image_resource
                    .as_ref()
                    .map(ResourceLease::handle))
            && let (Some(pod), Some(lease)) = (&mut this.widget.image, &view.images.image_resource)
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_resource(&mut image, lease.clone());
            this.widget.image_size = view.image_size();
        }
        if let (Some(pod), Some(size)) = (&mut this.widget.image, view.images.tiled_size)
            && this.widget.projection.images != view.images
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_tiles(&mut image, size, view.images.tiles.clone());
            this.widget.image_size = size;
        }
        if this.widget.node.kind != view.node.kind
            && let (
                Some(pod),
                NodeKind::Image {
                    fit,
                    slice,
                    slice_scale,
                    repeat,
                    tile_scale,
                    corner_colors,
                    adjustment,
                    slice_center,
                    source_rect,
                    alpha_layout,
                    sampling,
                    ..
                },
            ) = (&mut this.widget.image, &view.node.kind)
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            if !matches!(&this.widget.node.kind, NodeKind::Image { fit: old, .. } if old == fit) {
                image.insert_prop(*fit);
            }
            // This also clears slicing when a whole image switches to tiles.
            Image::set_source_rect(&mut image, *source_rect);
            Image::set_alpha_layout(&mut image, *alpha_layout);
            Image::set_sampling(&mut image, *sampling);
            Image::set_slice(&mut image, *slice, *slice_scale, *slice_center);
            Image::set_pattern(&mut image, *repeat, *tile_scale, *corner_colors);
            Image::set_color_adjustment(&mut image, *adjustment);
        }
        let old_size = this.widget.image_size;
        let new_size = view.image_size();
        if matches!(&view.node.kind, NodeKind::Image { .. })
            && [old_size.width as f32, old_size.height as f32]
                != [new_size.width as f32, new_size.height as f32]
        {
            this.widget.image_size = new_size;
            this.ctx.request_layout();
        }
        let caption_changed = match (&this.widget.node.kind, &view.node.kind) {
            (NodeKind::Text(a), NodeKind::Text(b)) => a != b,
            _ => Self::visual_caption(&this.widget.node) != Self::visual_caption(&view.node),
        };
        if caption_changed {
            this.ctx.request_layout();
        }
        if let Some(pod) = &mut this.widget.label
            && let Some(mut label) = this.ctx.get_mut(pod)
        {
            if caption_changed {
                Label::set_rich_text(
                    &mut label,
                    match &view.node.kind {
                        NodeKind::Text(text) => text.clone(),
                        _ => Self::visual_caption(&view.node).unwrap_or("").into(),
                    },
                );
            }
            if caption_changed || this.widget.node.props.reveal != view.node.props.reveal {
                Label::set_visible_bytes(&mut label, Self::reveal_range(&view.node));
            }
            if caption_changed
                || Self::reveal_fade(&this.widget.node) != Self::reveal_fade(&view.node)
            {
                Label::set_reveal_fade(&mut label, Self::reveal_fade(&view.node));
            }
            if this.widget.node.props.font_size != view.node.props.font_size {
                Label::insert_style(
                    &mut label,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font_weight != view.node.props.font_weight {
                Label::insert_style(
                    &mut label,
                    StyleProperty::FontWeight(reel_ui::reel_text::FontWeight::new(
                        view.node.props.font_weight as f32,
                    )),
                );
            }
            if this.widget.node.props.letter_spacing != view.node.props.letter_spacing {
                Label::insert_style(
                    &mut label,
                    StyleProperty::LetterSpacing(view.node.props.letter_spacing as f32),
                );
            }
            if this.widget.node.props.word_spacing != view.node.props.word_spacing {
                Label::insert_style(
                    &mut label,
                    StyleProperty::WordSpacing(view.node.props.word_spacing as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                Label::insert_style(&mut label, font_style(&view.node.props.font));
            }
            if this.widget.node.props.writing_mode != view.node.props.writing_mode {
                Label::set_writing_mode(&mut label, view.node.props.writing_mode);
            }
            if this.widget.node.props.text_align != view.node.props.text_align {
                Label::set_text_alignment(&mut label, view.node.props.text_align);
            }
            if text_line_breaking(&this.widget.node) != text_line_breaking(&view.node) {
                label.insert_prop(text_line_breaking(&view.node));
            }
            Label::set_text_fit(&mut label, view.node.props.text_fit);
            Label::set_outline(&mut label, text_outline(&view.node.props));
            Label::set_shadow(&mut label, text_shadow(&view.node.props));
            Label::set_gradient(&mut label, view.node.props.text_gradient);
            if this.widget.node.props.line_height != view.node.props.line_height {
                Label::insert_style(
                    &mut label,
                    StyleProperty::LineHeight(reel_ui::reel_text::LineHeight::FontSizeRelative(
                        view.node.props.line_height as f32,
                    )),
                );
            }
            if this.widget.node.props.color != view.node.props.color {
                label.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
        }
        if order_changed {
            let mut retained: BTreeMap<_, _> = std::mem::take(&mut this.widget.children)
                .into_iter()
                .map(|(node, pod)| (node.id, (node, pod)))
                .collect();
            this.widget.children = view
                .physical_children()
                .map(|next| {
                    retained
                        .remove(&next.node.id)
                        .unwrap_or_else(|| (next.node.clone(), Self::new_pod(next, nodes)))
                })
                .collect();
            for (id, (_, pod)) in retained {
                if let Some(old) = this
                    .widget
                    .projection
                    .children
                    .iter()
                    .find(|old| old.node.id == id)
                {
                    Self::forget(old, nodes);
                }
                this.ctx.remove_child(pod);
            }
            this.ctx.children_changed();
            this.ctx.set_interactive(
                this.widget.accepts_pointer_interaction(),
                this.widget.accepts_focus(),
            );
            this.ctx.request_cursor_icon_change();
        }
        if children_changed {
            for ((node, pod), next) in this
                .widget
                .children
                .iter_mut()
                .zip(view.physical_children())
            {
                if node.props.visible != next.node.props.visible
                    || node.props.flex != next.node.props.flex
                    || node.props.stretch != next.node.props.stretch
                    || node.props.align_x != next.node.props.align_x
                    || node.props.align_y != next.node.props.align_y
                {
                    this.ctx.request_layout();
                }
                if *node != next.node {
                    if transform_changed(&node.props, &next.node.props) {
                        this.ctx.request_compose();
                    }
                    *node = next.node.clone();
                }
                let registered = if let Some(mut child) = this.ctx.get_mut(pod) {
                    content_changed |= Self::sync(&mut child, next, feedback, nodes);
                    true
                } else {
                    false
                };
                if registered {
                    this.ctx.set_stashed(pod, !next.node.props.visible);
                }
            }
        }
        if let (
            NodeKind::Scroll {
                offset: previous, ..
            },
            NodeKind::Scroll { offset, .. },
        ) = (&this.widget.node.kind, &view.node.kind)
            && previous != offset
        {
            this.widget.scroll_baseline = *offset;
            this.widget.scroll_request =
                (*offset != this.widget.scroll.offset()).then_some(*offset);
            this.ctx.request_compose();
        }
        if let (
            NodeKind::Scroll {
                overscroll: old, ..
            },
            NodeKind::Scroll {
                overscroll: new, ..
            },
        ) = (&this.widget.node.kind, &view.node.kind)
            && old != new
        {
            this.widget.scroll.set_overscroll(*new);
            this.ctx.invalidate_input_configuration();
            this.ctx.request_compose();
        }
        if this.widget.node != view.node {
            let old = &this.widget.node.props;
            let new = &view.node.props;
            if old.width != new.width
                || old.flex != new.flex
                || old.height != new.height
                || old.max_width != new.max_width
                || old.max_height != new.max_height
                || old.stretch != new.stretch
                || old.gap != new.gap
                || old.padding != new.padding
                || old.font_size != new.font_size
                || old.font_weight != new.font_weight
                || old.letter_spacing != new.letter_spacing
                || old.word_spacing != new.word_spacing
                || old.font != new.font
                || old.writing_mode != new.writing_mode
                || old.text_fit != new.text_fit
                || text_line_breaking(&this.widget.node) != text_line_breaking(&view.node)
                || old.clip != new.clip
                || old.decorated != new.decorated
                || match (&this.widget.node.kind, &view.node.kind) {
                    (NodeKind::Row(a), NodeKind::Row(b))
                    | (NodeKind::Column(a), NodeKind::Column(b)) => a != b,
                    (NodeKind::Grid(a), NodeKind::Grid(b)) => a != b,
                    (NodeKind::VirtualList(a), NodeKind::VirtualList(b)) => a != b,
                    (NodeKind::Particles(a), NodeKind::Particles(b)) => {
                        a.batches != b.batches || a.size_scale != b.size_scale
                    }
                    (
                        NodeKind::Scroll {
                            axis: a,
                            follow_end: b,
                            align: c,
                            snap_y: g,
                            ..
                        },
                        NodeKind::Scroll {
                            axis: d,
                            follow_end: e,
                            align: f,
                            snap_y: h,
                            ..
                        },
                    ) => (a, b, c, g) != (d, e, f, h),
                    _ => false,
                }
            {
                this.ctx.request_layout();
            }
            if matches!(view.node.kind, NodeKind::Camera(_))
                && this.widget.node.kind != view.node.kind
            {
                this.ctx.request_compose();
            }
            if composite_changed(old, new) {
                this.ctx.request_pre_paint();
            }
            if decoration_changed(&this.widget.node, &view.node) {
                this.ctx.request_paint_only();
            }
            if this.widget.node.kind != view.node.kind {
                this.ctx.request_accessibility_update();
            }
            let accepted_focus = this.widget.accepts_focus();
            this.widget.node = view.node.clone();
            if accepted_focus != this.widget.accepts_focus() {
                this.ctx.set_interactive(
                    this.widget.accepts_pointer_interaction(),
                    this.widget.accepts_focus(),
                );
            }
        }
        if content_changed && !feedback && this.widget.is_button() {
            this.widget.button.cancel();
            this.ctx.invalidate_input_configuration();
        }
        this.widget.projection = view.clone();
        if matches!(this.widget.node.kind, NodeKind::VirtualList(_)) {
            this.widget
                .virtual_list
                .get_or_insert_with(Default::default);
        }
        this.widget.sync_dirty = false;
        content_changed
    }
    pub(super) fn interaction(
        &mut self,
        mut next: InteractionState,
        available: bool,
    ) -> Option<NodeEvent> {
        next.hover &= available;
        next.pressed = available && (next.pressed || self.button.is_pressed());
        next.focused &= available;
        next.focus_visible &= next.focused;
        if self.interaction == Some(next) {
            return None;
        }
        self.interaction = Some(next);
        Some(NodeEvent::Interaction(NodeInteraction {
            node: self.node.id,
            state: next,
        }))
    }
    pub(super) fn activate(&mut self) -> NodeEvent {
        if let NodeKind::Radio { revision, .. } = self.node.kind {
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Selection(revision),
                range: None,
            })
        } else if let NodeKind::Toggle { value, .. } = &mut Arc::make_mut(&mut self.node).kind {
            *value = !*value;
            let value = *value;
            self.sync_dirty = true;
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Bool(value),
                range: None,
            })
        } else {
            NodeEvent::Click(NodeClick { node: self.node.id })
        }
    }
    pub(super) fn sequential(&self, axis: Axis) -> bool {
        matches!(
            (&self.node.kind, axis),
            (
                NodeKind::Scroll {
                    axis: ScrollAxis::Horizontal,
                    ..
                },
                Axis::Horizontal
            ) | (
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical | ScrollAxis::Both,
                    ..
                },
                Axis::Vertical
            )
        )
    }
}

//! Measurement, layout and composition contexts.
use super::*;

// --- MARK: COMPUTE LENGTH
impl_context_method!(MeasureCtx<'_>, LayoutCtx<'_>, {
    /// Computes the `child`'s preferred border-box [`Length`] on the given `axis`.
    ///
    /// Container widgets usually call this method as part of their [`measure`] logic,
    /// to help them calculate their own length on the given `axis`. They call it as part
    /// of their [`layout`] logic if they have already chosen a length for one axis.
    /// Read [`measure`] and [`layout`] docs for more details about those processes.
    ///
    /// `auto_length` specifies the fallback behavior if the child's dimension is [`Dim::Auto`].
    /// If you're calling this from within [`measure`] then you usually want to derive
    /// this from `len_req`, probably using [`LenReq::reduce`], i.e. you would call
    /// `len_req.reduce(used_space_on_this_axis).into()`. However, if you're calling this
    /// from within [`layout`] then you usually want to use use [`LenDef::FitContent`]
    /// to ask the child to fit inside the available space. Sometimes a different fallback
    /// makes more sense, e.g. `Grid` uses [`LenDef::Fixed`] to fall back to the exact
    /// allocated child area size.
    ///
    /// `context_size` is the size that is used to resolve relative sizes.
    /// For example [`Ratio(0.5)`] will result in half the context size.
    /// This is usually the container widget's content-box size, i.e. excluding borders and padding.
    /// Examples of exceptions include `Grid` which will provide the child's area size,
    /// i.e. the union of cell sizes that the child occupies, and `Portal` which will provide
    /// its viewport size.
    ///
    /// `cross_length` is the length of the cross axis and is critical information for certain
    /// widgets, e.g. for text max advance or to keep an aspect ratio.
    ///
    /// [`measure`]: Widget::measure
    /// [`layout`]: Widget::layout
    /// [`Ratio(0.5)`]: crate::layout::Dim::Ratio
    /// [`Dim::Auto`]: crate::layout::Dim
    /// [`LenReq::reduce`]: crate::layout::LenReq::reduce
    pub fn compute_length(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        auto_length: LenDef,
        context_size: LayoutSize,
        axis: Axis,
        cross_length: Option<Length>,
    ) -> Length {
        let id = child.id();
        let node = self.children.item_mut(id).unwrap();
        resolve_length(
            self.global_state,
            self.property_arena,
            node,
            auto_length,
            context_size,
            axis,
            cross_length,
        )
    }
});

// --- MARK: MEASURE
impl MeasureCtx<'_> {
    /// Measure both axes using the same child cache and size resolution as layout.
    pub fn compute_size(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        auto_size: SizeDef,
        context_size: LayoutSize,
    ) -> Size {
        let node = self.children.item_mut(child.id()).unwrap();
        resolve_size(
            self.global_state,
            self.property_arena,
            node,
            auto_size,
            context_size,
        )
    }

    /// Returns the fallback [`LenDef`] of this measurement.
    ///
    /// This is essential information for widgets that want pass through measurements.
    /// That is, this fallback was chosen by widget A for widget B.
    /// Widget B can call this method and then pass it on via [`compute_length`] to widget C.
    /// Widget C will resolve its size with the fallback originally provided by widget A.
    ///
    /// Even easier, though, is to use [`redirect_measurement`].
    ///
    /// Calling `auto_length` will cause the result of this [`measure`] to not be cached.
    /// This is because `auto_length` is not part of the cache key.
    ///
    /// [`measure`]: Widget::measure
    /// [`compute_length`]: Self::compute_length
    /// [`redirect_measurement`]: Self::redirect_measurement
    pub fn auto_length(&mut self) -> LenDef {
        // We're adding a new variable, auto_length, into the measure function,
        // which is not part of the cache key. Hence, we need to not cache.
        self.cache_result = false;
        self.auto_length
    }

    /// Returns the context size of this measurement.
    ///
    /// This is usually the container widget's content-box size, i.e. excluding borders and padding.
    ///
    /// Examples of exceptions include `Grid` which will provide the child's area size,
    /// i.e. the union of cell sizes that the child occupies, and `Portal` which will provide
    /// its viewport size.
    ///
    /// The context size is used to resolve relative lengths, e.g. a width of [`Ratio(0.5)`]
    /// means half of this space. This resolving is done by Masonry and not manually in [`measure`].
    ///
    /// One or both lengths may be missing if they have not been computed yet,
    /// i.e. when the lengths depend on a child's size.
    ///
    /// This is essential information for widgets that want pass through measurements.
    /// That is, this context size was chosen by widget A for widget B.
    /// Widget B can call this method and then pass it on via [`compute_length`] to widget C.
    /// Widget C will resolve its size in relation to widget A.
    ///
    /// Even easier, though, is to use [`redirect_measurement`].
    ///
    /// Calling `context_size` will cause the result of this [`measure`] to not be cached.
    /// This is because `context_size` is not part of the cache key.
    ///
    /// [`measure`]: Widget::measure
    /// [`Ratio(0.5)`]: crate::layout::Dim::Ratio
    /// [`compute_length`]: Self::compute_length
    /// [`redirect_measurement`]: Self::redirect_measurement
    pub fn context_size(&mut self) -> LayoutSize {
        // We're adding a new variable, context_size, into the measure function,
        // which is not part of the cache key. Hence, we need to not cache.
        self.cache_result = false;
        self.context_size
    }

    /// Configures whether this [`measure`] result will be cached.
    ///
    /// Masonry will, by default, cache the results of measurement. The cache key is derived
    /// from `axis`, `len_req`, and `cross_length`. If the widget uses any other data to influence
    /// the result of the measurement, then the widget is responsible for requesting layout
    /// when any of that data changes. For properties, this is handled in [`property_changed`].
    /// For any other data you reference, the exact mechanism of detecting changes is up to you.
    /// If you can't detect changes of the referenced data, disable the cache with this method.
    ///
    /// [`measure`]: Widget::measure
    /// [`property_changed`]: Widget::property_changed
    pub fn cache_result(&mut self, cache_result: bool) {
        self.cache_result = cache_result;
    }

    /// Redirects the measurement request to a `child`.
    ///
    /// This is meant for thin wrapper widgets that want their children measured instead.
    ///
    /// It is a convenience wrapper over [`compute_length`] that automatically configures
    /// `auto_length` and `context_size` to whatever the outer container used. These could also
    /// be manually accessed via [`auto_length`] and [`context_size`] if you're so inclined.
    ///
    /// Calling `redirect_measurement` will cause the result of your [`measure`] to not be cached.
    /// The child's measurement might still be cached, depending on what policy the child chooses.
    /// This is because the redirection introduces new inputs in the form of [`auto_length`]
    /// and [`context_size`] that are not part of the cache key.
    ///
    /// [`measure`]: Widget::measure
    /// [`compute_length`]: Self::compute_length
    /// [`auto_length`]: Self::auto_length
    /// [`context_size`]: Self::context_size
    pub fn redirect_measurement(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        axis: Axis,
        cross_length: Option<Length>,
    ) -> Length {
        // We're adding two new variables, auto_length and context_size, into the measure function,
        // which are not part of the cache key. Hence, we need to not cache.
        self.cache_result = false;
        self.compute_length(
            child,
            self.auto_length,
            self.context_size,
            axis,
            cross_length,
        )
    }
}

// --- MARK: UPDATE LAYOUT
impl LayoutCtx<'_> {
    /// Window logical dimensions, independent of an authored camera or parent transform.
    pub fn window_size(&self) -> Size {
        let size = self
            .global_state
            .size
            .to_logical::<f64>(self.global_state.scale_factor);
        Size::new(size.width, size.height)
    }
    #[track_caller]
    fn assert_layout_done(&self, child: &WidgetPod<impl Widget + ?Sized>, method_name: &str) {
        let child_state = self.get_child_state(child);
        // If debug_assertions are enabled, then request_layout is always true until visited.
        let child_not_visited = cfg!(debug_assertions) && child_state.request_layout;
        if child_state.needs_layout() || child_not_visited {
            debug_panic!(
                "Error in {}: trying to call '{}' with child '{}' {} before computing its layout",
                self.widget_id(),
                method_name,
                self.get_child_dyn(child).short_type_name(),
                child.id(),
            );
        }
    }

    #[track_caller]
    fn assert_placed(&self, child: &WidgetPod<impl Widget + ?Sized>, method_name: &str) {
        if self.get_child_state(child).is_expecting_place_child_call {
            debug_panic!(
                "Error in {}: trying to call '{}' with child '{}' {} before placing it",
                self.widget_id(),
                method_name,
                self.get_child_dyn(child).short_type_name(),
                child.id(),
            );
        }
    }

    /// Computes the `child`'s preferred border-box size.
    ///
    /// The returned size will be finite, non-negative, and in logical pixels.
    ///
    /// Container widgets usually call this method as part of their [`layout`] logic, but
    /// ultimately they can disregard the result and pass a different size to [`run_layout`].
    /// Read [`layout`] docs for more details about that process.
    ///
    /// `auto_size` specifies the fallback behavior if the child has any dimension as [`Dim::Auto`].
    /// Most widgets should use [`SizeDef::fit`] to ask the child to fit inside the
    /// available space. However sometimes a different fallback makes more sense, e.g.
    /// `Grid` uses [`SizeDef::fixed`] to fall back to the exact allocated child area size.
    ///
    /// `context_size` is the size that is used to resolve relative sizes.
    /// For example [`Ratio(0.5)`] will result in half the context size.
    /// This is usually the container widget's content-box size, i.e. excluding borders and padding.
    /// Examples of exceptions include `Grid` which will provide the child's area size,
    /// i.e. the union of cell sizes that the child occupies, and `Portal` which will provide
    /// its viewport size.
    ///
    /// [`layout`]: Widget::layout
    /// [`Ratio(0.5)`]: crate::layout::Dim::Ratio
    /// [`Dim::Auto`]: crate::layout::Dim::Auto
    /// [`run_layout`]: Self::run_layout
    pub fn compute_size(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        auto_size: SizeDef,
        context_size: LayoutSize,
    ) -> Size {
        let id = child.id();
        let node = self.children.item_mut(id).unwrap();
        resolve_size(
            self.global_state,
            self.property_arena,
            node,
            auto_size,
            context_size,
        )
    }

    /// Lays out the `child` widget with a chosen border-box `size`.
    ///
    /// Container widgets must call this on every child as part of their [`layout`] method.
    ///
    /// The container widget should usually call [`compute_size`] on the `child`
    /// to get its preferred border-box `size`.
    /// However, ultimately the parent is in control and can choose any `size` for the child.
    ///
    /// If the chosen border-box `size` is smaller than what is required to fit the child's
    /// borders and padding, then the `size` will be expanded to meet those constraints.
    ///
    /// The provided `size` must be finite, non-negative, and in logical pixels.
    /// Non-finite or negative size will fall back to zero with a logged warning.
    ///
    /// # Panics
    ///
    /// Panics if the provided `size` is non-finite or negative and debug assertions are enabled.
    ///
    /// [`layout`]: Widget::layout
    /// [`compute_size`]: Self::compute_size
    pub fn run_layout(&mut self, child: &mut WidgetPod<impl Widget + ?Sized>, chosen_size: Size) {
        let id = child.id();
        let node = self.children.item_mut(id).unwrap();

        run_layout_on(self.global_state, self.property_arena, node, chosen_size);

        let state_mut = &mut self.children.item_mut(id).unwrap().item.state;
        self.widget_state.merge_up(state_mut);
    }

    /// Sets the position of the `child` widget, in this widget's content-box coordinate space.
    ///
    /// Container widgets must call this method with each non-stashed child in their
    /// [`layout`] method, after calling `ctx.run_layout(child, size)`.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] has not been called yet for the child.
    ///
    /// [`layout`]: Widget::layout
    #[track_caller]
    pub fn place_child(&mut self, child: &mut WidgetPod<impl Widget + ?Sized>, origin: Point) {
        self.assert_layout_done(child, "place_child");
        if origin.x.is_nan()
            || origin.x.is_infinite()
            || origin.y.is_nan()
            || origin.y.is_infinite()
        {
            debug_panic!(
                "Error in {}: trying to call 'place_child' with child '{}' {} with invalid origin {:?}",
                self.widget_id(),
                self.get_child_dyn(child).short_type_name(),
                child.id(),
                origin,
            );
        }

        // Convert child's origin from this widget's content-box space to border-box space.
        let translation = self.widget_state.border_box_translation();
        let child_origin = origin + translation;
        let child_state = self.get_child_state_mut(child);

        place_widget(child_state, child_origin);
    }

    /// Sets explicit paint [`Insets`] for this widget.
    ///
    /// The argument is an [`Insets`] struct that indicates where your widget will overpaint,
    /// relative to its layout content-box, as defined by the `size` given to the widget's
    /// [`layout`] method.
    ///
    /// You are only required to notify of painting that actually overflows the layout border-box.
    /// The insets will still be relative to the content-box, it's just that Masonry doesn't
    /// really need to be notified if you're just painting over your padding or borders.
    ///
    /// You are only required to notify of painting done directly by this widget.
    /// Child widget overdraw needs to be reported by those child widgets themselves.
    ///
    /// [`layout`]: Widget::layout
    pub fn set_paint_insets(&mut self, insets: impl Into<Insets>) {
        let insets = insets.into();
        let insets = Insets::new(
            insets.x0 - self.widget_state.border_box_insets.x0,
            insets.y0 - self.widget_state.border_box_insets.y0,
            insets.x1 - self.widget_state.border_box_insets.x1,
            insets.y1 - self.widget_state.border_box_insets.y1,
        );
        self.widget_state.paint_box_insets = insets.nonnegative();
    }

    /// Sets explicit baselines for this widget.
    ///
    /// The baseline positions are used to align widgets that contain text,
    /// such as buttons, labels, and other controls. It may also be used
    /// by other widgets that are opinionated about how they are aligned
    /// relative to neighbouring text, such as switches or checkboxes.
    ///
    /// The provided values must be the distance from the top of this
    /// widget's content-box to its baseline. If there aren't multiple
    /// baselines then set the same baseline as both `first` and `last`.
    ///
    /// Most container widgets can use [`derive_baselines`] instead.
    /// Multi-child containers should derive their baselines using [`child_aligned_baselines`].
    ///
    /// [`derive_baselines`]: Self::derive_baselines
    /// [`child_aligned_baselines`]: Self::child_aligned_baselines
    pub fn set_baselines(&mut self, first_baseline: f64, last_baseline: f64) {
        self.widget_state.first_baseline = first_baseline + self.widget_state.border_box_insets.y0;
        self.widget_state.last_baseline = last_baseline + self.widget_state.border_box_insets.y0;
    }

    /// Sets explicit baselines for this widget such that they match the child's aligned baselines.
    ///
    /// Most container widgets should use this method to derive their baselines from their child.
    /// More complex containers can use [`child_aligned_baselines`] in multi-child scenarios.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] or [`LayoutCtx::place_child`]
    /// have not been called yet for the child.
    ///
    /// [`child_aligned_baselines`]: Self::child_aligned_baselines
    #[track_caller]
    pub fn derive_baselines(&mut self, child: &WidgetPod<impl Widget + ?Sized>) {
        self.assert_layout_done(child, "derive_baselines");
        self.assert_placed(child, "derive_baselines");

        let child_state = self.get_child_state(child);
        let first_baseline = child_state.origin.y + child_state.aligned_first_baseline();
        let last_baseline = child_state.origin.y + child_state.aligned_last_baseline();
        self.widget_state.first_baseline = first_baseline;
        self.widget_state.last_baseline = last_baseline;
    }

    /// Clears explicitly set baseline positions for this widget.
    ///
    /// This results in the effective baseline being the bottom edge of this widget's border-box.
    pub fn clear_baselines(&mut self) {
        self.widget_state.first_baseline = f64::NAN;
        self.widget_state.last_baseline = f64::NAN;
    }

    /// Returns the insets for converting between content-box and border-box rects.
    ///
    /// Add these insets to the content-box to get the border-box,
    /// and subtract these insets from the border-box to get the content-box.
    pub fn border_box_insets(&mut self) -> Insets {
        self.widget_state.border_box_insets
    }

    /// Returns whether this widget needs to call [`LayoutCtx::run_layout`].
    pub fn needs_layout(&self) -> bool {
        self.widget_state.needs_layout()
    }

    /// Returns whether a child of this widget needs to call [`LayoutCtx::run_layout`].
    pub fn child_needs_layout(&self, child: &WidgetPod<impl Widget + ?Sized>) -> bool {
        self.get_child_state(child).needs_layout()
    }

    /// Returns the `child` widget's `(first, last)` layout baselines.
    ///
    /// The distances are from the top of the child widget's layout border-box to its baseline.
    ///
    /// Call this if the child's baseline plays a role in choosing its placement.
    /// For deriving this widget's baselines call [`child_aligned_baselines`] instead,
    /// or better yet use [`derive_baselines`] if possible.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] has not been called yet for
    /// the child.
    ///
    /// [`child_aligned_baselines`]: Self::child_aligned_baselines
    /// [`derive_baselines`]: Self::derive_baselines
    #[track_caller]
    pub fn child_layout_baselines(&self, child: &WidgetPod<impl Widget + ?Sized>) -> (f64, f64) {
        self.assert_layout_done(child, "child_layout_baselines");

        let child_state = self.get_child_state(child);
        (
            child_state.layout_first_baseline(),
            child_state.layout_last_baseline(),
        )
    }

    /// Returns the `child` widget's `(first, last)` aligned baselines.
    ///
    /// The distances are from the top of the child widget's aligned border-box to its baseline.
    ///
    /// This aligned version should be used for deriving this widget's own baselines based
    /// on the child's baselines. That is if [`derive_baselines`] can't be used.
    /// For deciding where to place the child based on its baselines,
    /// you need to use [`child_layout_baselines`] instead.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] or [`LayoutCtx::place_child`]
    /// have not been called yet for the child.
    ///
    /// [`derive_baselines`]: Self::derive_baselines
    /// [`child_layout_baselines`]: Self::child_layout_baselines
    #[track_caller]
    pub fn child_aligned_baselines(&self, child: &WidgetPod<impl Widget + ?Sized>) -> (f64, f64) {
        self.assert_layout_done(child, "child_aligned_baselines");
        self.assert_placed(child, "child_aligned_baselines");

        let child_state = self.get_child_state(child);
        (
            child_state.aligned_first_baseline(),
            child_state.aligned_last_baseline(),
        )
    }

    /// Returns the given child's aligned border-box origin
    /// in this widget's content-box coordinate space.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] or [`LayoutCtx::place_child`]
    /// have not been called yet for the child.
    #[track_caller]
    pub fn child_origin(&self, child: &WidgetPod<impl Widget + ?Sized>) -> Point {
        self.assert_layout_done(child, "child_origin");
        self.assert_placed(child, "child_origin");
        self.get_child_state(child).origin - self.widget_state.border_box_translation()
    }

    /// Returns the given child's layout border-box size.
    ///
    /// # Panics
    ///
    /// This method will panic if [`LayoutCtx::run_layout`] has not been called yet for
    /// the child.
    #[track_caller]
    pub fn child_size(&self, child: &WidgetPod<impl Widget + ?Sized>) -> Size {
        self.assert_layout_done(child, "child_size");
        self.get_child_state(child).layout_border_box_size
    }

    /// Sets the widget's clip path in the widget's content-box coordinate space.
    ///
    /// A widget's clip path will have two effects:
    /// - It serves as a mask for painting operations of this widget and its children.
    ///   Note that while all painting done by children will be clipped by this path,
    ///   only the painting done in [`paint`] by this widget itself will be clipped.
    ///   The remaining painting done in [`pre_paint`] and [`post_paint`] will not be clipped.
    /// - Pointer events must be inside this path to reach the widget's children.
    ///
    /// [`paint`]: Widget::paint
    /// [`pre_paint`]: Widget::pre_paint
    /// [`post_paint`]: Widget::post_paint
    pub fn set_clip_path(&mut self, path: Rect) {
        // Translate the clip path to the widget's border-box coordinate space.
        let path = path + self.widget_state.border_box_translation();
        // We intentionally always log this because clip paths are:
        // 1) Relatively rare in the tree
        // 2) An easy potential source of items not being visible when expected
        trace!("set_clip_path {path:?}");
        self.widget_state.clip_path = Some(path);
        // TODO - Updating the clip path may have
        // other knock-on effects we'd need to document.
        self.widget_state.request_accessibility = true;
        self.widget_state.needs_accessibility = true;
        self.widget_state.needs_paint = true;
    }

    /// Removes the widget's clip path.
    ///
    /// See [`LayoutCtx::set_clip_path`] for details.
    pub fn clear_clip_path(&mut self) {
        trace!("clear_clip_path");
        self.widget_state.clip_path = None;
        // TODO - Updating the clip path may have
        // other knock-on effects we'd need to document.
        self.widget_state.request_accessibility = true;
        self.widget_state.needs_accessibility = true;
        self.widget_state.needs_paint = true;
    }
}

impl ComposeCtx<'_> {
    /// Maps a child's content-box coordinates into this widget's content-box
    /// coordinates using its current layout origin and affine transform.
    /// Scroll translation is excluded so viewport consumers can supply their
    /// logical scroll position independently of transient visual overscroll.
    pub fn child_layout_transform(&self, child: &WidgetPod<impl Widget + ?Sized>) -> Affine {
        let child = self.get_child_state(child);
        child
            .transform
            .pre_translate(child.border_box_translation())
            .then_translate(child.origin.to_vec2() - self.widget_state.border_box_translation())
    }

    /// The child's untransformed layout size, for size-dependent composition.
    pub fn child_size(&self, child: &WidgetPod<impl Widget + ?Sized>) -> Size {
        self.get_child_state(child).layout_border_box_size
    }

    /// Update a child's local affine after layout, before composing its subtree.
    /// Paint, accessibility and presented input geometry use this same transform.
    pub fn set_child_transform(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        transform: Affine,
    ) {
        let child = self.get_child_state_mut(child);
        if child.transform != transform {
            child.transform = transform;
            child.transform_changed = true;
            child.needs_compose = true;
        }
    }

    /// Sets the scroll translation for the child widget.
    ///
    /// The translation is applied on top of the position from [`LayoutCtx::place_child`].
    ///
    /// The given translation may be quantized so the child's final position
    /// stays pixel-perfect.
    pub fn set_child_scroll_translation(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        translation: Vec2,
    ) {
        if translation.x.is_nan()
            || translation.x.is_infinite()
            || translation.y.is_nan()
            || translation.y.is_infinite()
        {
            debug_panic!(
                "Error in {}: trying to call 'set_child_scroll_translation' with child '{}' {} with invalid translation {:?}",
                self.widget_id(),
                self.get_child_dyn(child).short_type_name(),
                child.id(),
                translation,
            );
        }

        let translation = translation.round();

        let child = self.get_child_state_mut(child);
        if translation != child.scroll_translation {
            child.scroll_translation = translation;
            child.transform_changed = true;
            child.needs_compose = true;
        }
    }

    /// Sets the scroll translation for the child widget.
    ///
    /// The translation is applied on top of the position from [`LayoutCtx::place_child`].
    ///
    /// Unlike [`Self::set_child_scroll_translation`], doesn't perform pixel-snapping.
    /// This method should be used for intermediary scroll values during scroll animations.
    pub fn set_animated_child_scroll_translation(
        &mut self,
        child: &mut WidgetPod<impl Widget + ?Sized>,
        translation: Vec2,
    ) {
        if translation.x.is_nan()
            || translation.x.is_infinite()
            || translation.y.is_nan()
            || translation.y.is_infinite()
        {
            debug_panic!(
                "Error in {}: trying to call 'set_animated_child_scroll_translation' with child '{}' {} with invalid translation {:?}",
                self.widget_id(),
                self.get_child_dyn(child).short_type_name(),
                child.id(),
                translation,
            );
        }

        let child = self.get_child_state_mut(child);
        if translation != child.scroll_translation {
            child.scroll_translation = translation;
            child.transform_changed = true;
            child.needs_compose = true;
        }
    }
}

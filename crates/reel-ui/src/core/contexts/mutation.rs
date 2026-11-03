//! Widget mutation, child registration and paint contexts.
use super::*;

// --- MARK: UPDATE FLAGS
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    RawCtx<'_>,
    {
        /// Invalidate interactions recorded with an earlier control configuration.
        /// Value and layout updates must not call this.
        pub fn invalidate_input_configuration(&mut self) {
            self.global_state.invalidate_geometry();
            self.widget_state.input_revision = self.widget_state.input_revision.wrapping_add(1);
            self.global_state.needs_pointer_pass = true;
            self.request_render();
        }

        /// Update a widget whose content adds or removes interactive regions.
        pub fn set_interactive(&mut self, pointer: bool, focus: bool) {
            let pointer = pointer && !self.widget_state.in_cursor;
            let focus = focus && !self.widget_state.in_cursor;
            if self.widget_state.accepts_pointer_interaction == pointer
                && self.widget_state.accepts_focus == focus
            {
                return;
            }
            self.widget_state.accepts_pointer_interaction = pointer;
            self.widget_state.accepts_focus = focus;
            self.widget_state.needs_update_focusable = true;
            if !focus && self.global_state.next_focused_widget == Some(self.widget_state.id) {
                self.global_state.next_focused_widget = None;
            }
            self.invalidate_input_configuration();
        }

        /// Requests a [`paint`](crate::core::Widget::paint) and an
        /// [`accessibility`](crate::core::Widget::accessibility) pass.
        pub fn request_render(&mut self) {
            trace!("request_render");
            self.widget_state.request_pre_paint = true;
            self.widget_state.request_paint = true;
            self.widget_state.request_post_paint = true;
            self.widget_state.needs_paint = true;
            self.widget_state.needs_accessibility = true;
            self.widget_state.request_accessibility = true;
        }

        /// Requests a paint pass for the [`pre_paint`](crate::core::Widget::pre_paint) method.
        pub fn request_pre_paint(&mut self) {
            trace!("request_pre_paint");
            self.widget_state.request_pre_paint = true;
            self.widget_state.needs_paint = true;
        }

        /// Requests a paint pass, specifically for the [`paint`] method.
        ///
        /// Unlike [`request_render`], this does not request an [`accessibility`] pass
        /// or a call to [`pre_paint`] or [`post_paint`].
        ///
        /// Use `request_render` unless you're sure those aren't needed.
        ///
        /// [`paint`]: crate::core::Widget::paint
        /// [`request_render`]: Self::request_render
        /// [`accessibility`]: crate::core::Widget::accessibility
        /// [`pre_paint`]: crate::core::Widget::post_paint
        /// [`post_paint`]: crate::core::Widget::post_paint
        pub fn request_paint_only(&mut self) {
            trace!("request_paint_only");
            self.widget_state.request_paint = true;
            self.widget_state.needs_paint = true;
        }

        /// Requests a paint pass for the [`post_paint`](crate::core::Widget::post_paint) method.
        pub fn request_post_paint(&mut self) {
            trace!("request_post_paint");
            self.widget_state.request_post_paint = true;
            self.widget_state.needs_paint = true;
        }

        /// Requests an [`accessibility`](crate::core::Widget::accessibility) pass.
        ///
        /// This doesn't request a [`paint`](crate::core::Widget::paint) pass.
        /// If you want to request both an accessibility pass and a paint pass,
        /// use [`request_render`](Self::request_render).
        pub fn request_accessibility_update(&mut self) {
            trace!("request_accessibility_update");
            self.widget_state.needs_accessibility = true;
            self.widget_state.request_accessibility = true;
        }

        /// Requests a [`layout`] pass.
        ///
        /// Call this method if the widget has changed in a way that requires a layout pass.
        ///
        /// [`layout`]: crate::core::Widget::layout
        pub fn request_layout(&mut self) {
            trace!("request_layout");
            self.widget_state.request_layout = true;
            self.widget_state.set_needs_layout(true);
        }

        /// Requests that this widget's [`compose`] method be called.
        ///
        /// The compose pass is often cheaper than the layout pass,
        /// because it can only transform individual widgets' position.
        /// Use this when widget-owned state read by [`compose`] changes,
        /// such as a scroll offset applied to a child during compose.
        ///
        /// [`compose`]: crate::core::Widget::compose
        pub fn request_compose(&mut self) {
            trace!("request_compose");
            self.widget_state.needs_compose = true;
            self.widget_state.request_compose = true;
        }

        /// Requests an animation frame.
        pub fn request_anim_frame(&mut self) {
            trace!("request_anim_frame");
            self.widget_state.request_anim = true;
            self.widget_state.needs_anim = true;
        }

        /// Notifies Masonry that the cursor returned by [`Widget::get_cursor`] has changed.
        ///
        /// This is mostly meant for cases where the cursor changes even if the pointer doesn't
        /// move, because the nature of the widget has changed somehow.
        pub fn request_cursor_icon_change(&mut self) {
            trace!("request_cursor_icon_change");
            self.global_state.needs_pointer_pass = true;
        }

        /// Indicates that your children have changed.
        ///
        /// Widgets must call this method after adding a new child.
        ///
        /// This method will also call [`request_layout`](Self::request_layout).
        pub fn children_changed(&mut self) {
            trace!("children_changed");
            self.global_state.modal_dirty = true;
            self.global_state.input_layer_order_dirty = true;
            self.widget_state.children_changed = true;
            self.widget_state.needs_update_focusable = true;
            self.request_layout();
        }

        /// Indicates that a child is about to be removed from the tree.
        ///
        /// Container widgets should avoid dropping `WidgetPod`s. Instead, they should
        /// pass them to this method.
        ///
        /// This method will also call [`children_changed`](Self::children_changed).
        pub fn remove_child(&mut self, child: WidgetPod<impl Widget + ?Sized>) {
            fn remove_node(
                global_state: &mut RenderRootState,
                parent_state: &mut WidgetState,
                property_arena: &PropertyArena,
                view: crate::app::GeometryView<'_>,
                node: ArenaMut<'_, WidgetArenaNode>,
            ) {
                let mut children = node.children;
                let widget_type_id = (*node.item.widget).type_id();
                let widget = &mut *node.item.widget;
                let state = &mut node.item.state;

                let pointer_count = global_state.other_pointers.len();
                for index in 0..=pointer_count {
                    if index < pointer_count {
                        global_state.swap_pointer(index);
                    }
                    if global_state.pointer.pointer_capture_target == Some(state.id)
                        && global_state.pointer.pointer_capture_terminal_delivered != Some(state.id)
                    {
                        global_state.pointer.pointer_capture_terminal_delivered = Some(state.id);
                        let stack = property_arena.get(state.property_stack_id, widget_type_id);
                        let mut props = PropertiesMut {
                            local: &mut node.item.properties,
                            default_map: property_arena
                                .default_properties
                                .for_widget(widget_type_id),
                            stack,
                            class_set: &node.item.class_set,
                        };
                        let target = state.id;
                        if let Some(geometry) = view.bind(target) {
                            let mut ctx = EventCtx {
                                geometry,
                                global_state,
                                widget_state: state,
                                children: children.reborrow_mut(),
                                property_arena,
                                target,
                                allow_pointer_capture: false,
                                is_handled: false,
                                propagation_stopped: false,
                            };
                            let info = ctx.global_state.pointer.info;
                            let dispatching = ctx.global_state.dispatching_pointer;
                            ctx.global_state.dispatching_pointer = true;
                            widget.on_pointer_event(
                                &mut ctx,
                                &mut props,
                                &crate::core::PointerEvent::Cancel(info),
                            );
                            ctx.global_state.dispatching_pointer = dispatching;
                        }
                    }
                    if global_state.pointer.pointer_capture_target == Some(state.id) {
                        global_state.pointer.pointer_capture_target = None;
                        global_state.pointer.pointer_capture_terminal_delivered = None;
                        global_state.needs_pointer_pass = true;
                    }

                    if index < pointer_count {
                        global_state.swap_pointer(index);
                    }
                }

                let parent_name = widget.short_type_name();
                let parent_id = state.id;
                for child_id in widget.children_ids() {
                    let Some(node) = children.item_mut(child_id) else {
                        panic!(
                            "Error in '{parent_name}' {parent_id}: cannot find child {child_id} returned by children_ids()"
                        );
                    };

                    remove_node(global_state, state, property_arena, view, node);
                }

                // If we remove the focus anchor, its parent becomes the anchor.
                if global_state.focus_anchor == Some(state.id) {
                    global_state.focus_anchor = Some(parent_state.id);
                }

                // Hover/active callbacks can retire the target before cursor refresh.
                if global_state.cursor.target == Some(state.id) {
                    global_state.cursor.target = None;
                    global_state.needs_pointer_pass = true;
                }

                global_state.scene_cache.remove(&state.id);
                global_state.registered_widgets -= 1;
                if let Some(index) = global_state
                    .input_layers
                    .iter()
                    .position(|id| *id == state.id)
                {
                    global_state.input_layers.remove(index);
                }

                if let Some(layers) = global_state.attached_layers.remove(&state.id) {
                    for (_, layer_id) in layers {
                        global_state.emit_signal(RenderRootSignal::RemoveLayer(layer_id));
                    }
                }
            }

            let id = child.id();
            let node = self
                .children
                .item_mut(id)
                .expect("remove_child: child not found");
            let source = self.global_state.input_geometry_source.clone();
            remove_node(
                self.global_state,
                self.widget_state,
                self.property_arena,
                match self.geometry {
                    crate::app::ContextGeometry::Live => source.borrow(),
                    crate::app::ContextGeometry::Presented { view, .. } => {
                        crate::app::GeometryView::Presented(Some(view))
                    }
                },
                node,
            );

            let _ = self.children.remove(id).unwrap();

            self.children_changed();
        }

        /// Sets the disabled state for this widget.
        ///
        /// Setting this to `false` does not mean a widget is not still disabled; for instance it may
        /// still be disabled by an ancestor. See [`is_disabled`] for more information.
        ///
        /// [`is_disabled`]: EventCtx::is_disabled
        pub fn set_disabled(&mut self, disabled: bool) {
            self.widget_state.needs_update_disabled = true;
            self.widget_state.is_explicitly_disabled = disabled;
        }

        /// Give this visible subtree exclusive input. Presented hosts apply
        /// the change when they accept the corresponding frame geometry.
        /// Disabling the subtree keeps the modal barrier; stashing releases it.
        pub fn set_modal(&mut self, modal: bool) {
            if self.widget_state.is_modal != modal {
                self.global_state.invalidate_geometry();
                self.widget_state.is_modal = modal;
                self.global_state.modal_dirty = true;
                self.request_render();
            }
        }

        /// Sets the local transform for this widget.
        ///
        /// This maps this widget's border-box coordinate space
        /// to the parent's border-box coordinate space.
        ///
        /// It behaves similarly as CSS transforms.
        pub fn set_transform(&mut self, transform: Affine) {
            self.widget_state.transform = transform;
            self.widget_state.transform_changed = true;
            self.widget_state.needs_compose = true;
        }

        /// Adds a string to this widget's [class set].
        ///
        /// Changes will be applied in the next update pass and may affect property resolution.
        ///
        /// [class]: crate::doc::concepts#classes
        pub fn add_class(&mut self, class: &str) {
            self.widget_state.class_diff.add(class);
            self.widget_state.request_update_props = true;
            self.widget_state.needs_update_props = true;
        }

        /// Removes a string from this widget's [class set].
        ///
        /// Changes will be applied in the next update pass and may affect property resolution.
        ///
        /// [class]: crate::doc::concepts#classes
        pub fn remove_class(&mut self, class: &str) {
            self.widget_state.class_diff.remove(class);
            self.widget_state.request_update_props = true;
            self.widget_state.needs_update_props = true;
        }
    }
);

// --- MARK: OTHER METHODS
// Methods on mutable context types
impl_context_method!(
    MutateCtx<'_>,
    ActionCtx<'_>,
    EventCtx<'_>,
    UpdateCtx<'_>,
    MeasureCtx<'_>,
    LayoutCtx<'_>,
    ComposeCtx<'_>,
    RawCtx<'_>,
    {
        // TODO - Remove from MeasureCtx/LayoutCtx/ComposeCtx
        /// Marks child widget as stashed.
        ///
        /// If `stashed` is true, the child will not be painted or listed in the accessibility tree.
        ///
        /// This will *not* trigger a layout pass.
        pub fn set_stashed(&mut self, child: &mut WidgetPod<impl Widget + ?Sized>, stashed: bool) {
            let child_state = self.get_child_state_mut(child);
            // Stashing is generally a property derived from the parent widget's state
            // (rather than set imperatively), so it is likely to be set as part of passes.
            // Therefore, we avoid re-running the update_stashed_pass in most cases.
            if child_state.is_explicitly_stashed != stashed {
                child_state.is_explicitly_stashed = stashed;
                child_state.needs_update_stashed = true;
                self.widget_state.needs_update_stashed = true;
                self.global_state.modal_dirty = true;
            }
        }

        // TODO - Remove from MutateCtx?
        /// Queues a callback that will be called with a [`WidgetMut`] for this widget.
        ///
        /// The callbacks will be run in the order they were submitted during the mutate pass.
        pub fn mutate_self_later(
            &mut self,
            f: impl FnOnce(WidgetMut<'_, dyn Widget>) + Send + 'static,
        ) {
            let callback = MutateCallback {
                id: self.widget_state.id,
                callback: Box::new(f),
            };
            self.global_state.mutate_callbacks.push(callback);
        }

        /// Queues a callback that will be called with a [`WidgetMut`] for the given child widget.
        ///
        /// The callbacks will be run in the order they were submitted during the mutate pass.
        pub fn mutate_child_later<W: Widget + FromDynWidget + ?Sized>(
            &mut self,
            child: &mut WidgetPod<W>,
            f: impl FnOnce(WidgetMut<'_, W>) + Send + 'static,
        ) {
            let callback = MutateCallback {
                id: child.id(),
                callback: Box::new(|mut widget_mut| f(widget_mut.downcast())),
            };
            self.global_state.mutate_callbacks.push(callback);
        }

        /// Queues a callback that will be called with a [`WidgetMut`] for the widget with the given id.
        ///
        /// The callbacks will be run in the order they were submitted during the mutate pass.
        pub fn mutate_later(
            &mut self,
            // TODO - Use WidgetTag instead?
            target: WidgetId,
            f: impl FnOnce(WidgetMut<'_, dyn Widget>) + Send + 'static,
        ) {
            let callback = MutateCallback {
                id: target,
                callback: Box::new(f),
            };
            self.global_state.mutate_callbacks.push(callback);
        }

        /// Returns direct reference to the stored child, and a context handle for that child.
        pub fn get_raw<Child: Widget + FromDynWidget + ?Sized>(
            &mut self,
            child: &mut WidgetPod<Child>,
        ) -> Option<(&Child, RawCtx<'_>)> {
            let geometry = self.geometry.child(child.id())?;
            let node_mut = self.children.item_mut(child.id())?;
            let child_ctx = RawCtx {
                geometry,
                global_state: self.global_state,
                parent_widget_state: self.widget_state,
                widget_state: &mut node_mut.item.state,
                children: node_mut.children,
                property_arena: self.property_arena,
            };

            let widget = Child::from_dyn(&*node_mut.item.widget).unwrap();

            Some((widget, child_ctx))
        }

        /// Returns direct mutable reference to the stored child, and a context handle for that child.
        ///
        /// This context lets you set pass flags for the child widget, which may be tricky.
        /// In general, you should avoid setting the flags of a pass that runs before the
        /// pass you're currently in.
        /// Not doing so might lead to performance cliffs and, hypothetically, panics.
        ///
        /// This method is an escape hatch for cases where a parent widget completely
        /// controls their child, but needs it to be a separate widget for user interaction to
        /// behave as expected.
        /// As such, the child widget must opt-in using the `AllowRawMut` trait.
        ///
        /// See [pass documentation](crate::doc::pass_system) for the pass order.
        pub fn get_raw_mut<Child: Widget + FromDynWidget + AllowRawMut + ?Sized>(
            &mut self,
            child: &mut WidgetPod<Child>,
        ) -> Option<(&mut Child, RawCtx<'_>)> {
            let geometry = self.geometry.child(child.id())?;
            let node_mut = self.children.item_mut(child.id())?;
            let child_ctx = RawCtx {
                geometry,
                global_state: self.global_state,
                parent_widget_state: self.widget_state,
                widget_state: &mut node_mut.item.state,
                children: node_mut.children,
                property_arena: self.property_arena,
            };

            let widget = Child::from_dyn_mut(&mut *node_mut.item.widget).unwrap();

            Some((widget, child_ctx))
        }

        /// Submits an action, which indicates that this widget requires something be handled
        /// by the application, such as user input.
        ///
        /// The `Action` type parameter should always be the `Self::Action` associated type
        /// of the widget you're calling this method from.
        /// Masonry will validate this, and this method may panic if this isn't the case.
        ///
        /// For further details see [`ErasedAction`].
        pub fn submit_action<Action: AnyDebug + Send>(&mut self, action: impl Into<Action>) {
            trace!("submit_action");
            let action = action.into();
            if action.type_id() != self.widget_state.action_type {
                #[cfg(debug_assertions)]
                let expected_type = self.widget_state.action_type_name;
                #[cfg(not(debug_assertions))]
                let expected_type = "<Self as Widget>::Action";

                debug_panic!(
                    "Trying to emit action of incorrect type `{}`. Expected type is `{}`.",
                    action.type_name(),
                    expected_type,
                );
                return;
            }
            self.global_state
                .actions
                .push((Box::new(action), self.widget_state.id));
        }

        /// Submits a type-erased action.
        ///
        /// Unlike [`Self::submit_action`], this method lets you submit an action with an
        /// arbitrary type, which may not match `Self::Action`.
        /// This may act as an escape hatch in some situations.
        ///
        /// For further details see [`ErasedAction`].
        pub fn submit_untyped_action(&mut self, action: ErasedAction) {
            trace!("submit_untyped_action");
            self.global_state
                .actions
                .push((action, self.widget_state.id));
        }

        /// Sets the IME cursor area in the widget's content-box coordinate space.
        ///
        /// When this widget is [focused] and [accepts text input], the reported IME area is sent
        /// to the platform. The area can be used by the platform to, for example, place a
        /// candidate box near that area, while ensuring the area is not obscured.
        ///
        /// If no IME area is set, then Masonry will use the widget's aligned border-box rect.
        ///
        /// [focused]: EventCtx::request_focus
        /// [accepts text input]: Widget::accepts_text_input
        pub fn set_ime_area(&mut self, ime_area: Rect) {
            let translation = self.widget_state.border_box_translation();
            self.widget_state.ime_area = Some(ime_area + translation);
        }

        /// Removes the IME cursor area.
        ///
        /// See [`set_ime_area`] for more details.
        ///
        /// [`set_ime_area`]: Self::set_ime_area
        pub fn clear_ime_area(&mut self) {
            self.widget_state.ime_area = None;
        }

        /// Enables or disables this editor's system IME without changing focus or keyboard ownership.
        /// Discard local preedit before disabling. Unfocused widgets retain the setting for focus.
        pub fn set_ime_enabled(&mut self, enabled: bool) {
            self.widget_state.ime_enabled = enabled;
            if self.global_state.focused_widget != Some(self.widget_state.id)
                || !self.widget_state.accepts_text_input
                || self.global_state.is_ime_active == enabled
            {
                return;
            }
            self.global_state.is_ime_active = enabled;
            self.global_state.emit_signal(if enabled {
                RenderRootSignal::StartIme
            } else {
                RenderRootSignal::EndIme
            });
            if enabled {
                self.global_state
                    .emit_signal(RenderRootSignal::new_ime_moved_signal(
                        self.widget_state.get_ime_area(),
                    ));
            }
        }

        /// Restart the focused editor's system IME after replacing its value or selection.
        /// Local preedit must be discarded before calling this method.
        pub fn reset_ime(&mut self) {
            if self.global_state.is_ime_active
                && self.global_state.focused_widget == Some(self.widget_state.id)
            {
                self.global_state.emit_signal(RenderRootSignal::EndIme);
                self.global_state.emit_signal(RenderRootSignal::StartIme);
                self.global_state
                    .emit_signal(RenderRootSignal::new_ime_moved_signal(
                        self.widget_state.get_ime_area(),
                    ));
            }
        }

        /// Sets the contents of the platform clipboard.
        ///
        /// For example, text widgets should call this for "cut" and "copy" user interactions.
        /// Note that we currently don't support the "Primary" selection buffer on X11/Wayland.
        pub fn set_clipboard(&mut self, contents: String) {
            trace!("set_clipboard");
            self.global_state
                .emit_signal(RenderRootSignal::ClipboardStore(contents));
        }

        /// Starts a window drag.
        ///
        /// Moves the window with the left mouse button until the button is released.
        pub fn drag_window(&mut self) {
            trace!("drag_window");
            self.global_state.emit_signal(RenderRootSignal::DragWindow);
        }

        /// Starts a window resize.
        ///
        /// Resizes the window with the left mouse button until the button is released.
        pub fn drag_resize_window(&mut self, direction: ResizeDirection) {
            trace!("drag_resize_window");
            self.global_state
                .emit_signal(RenderRootSignal::DragResizeWindow(direction));
        }

        /// Toggles the maximized state of the window.
        pub fn toggle_maximized(&mut self) {
            trace!("toggle_maximized");
            self.global_state
                .emit_signal(RenderRootSignal::ToggleMaximized);
        }

        /// Minimizes the window.
        pub fn minimize(&mut self) {
            trace!("minimize");
            self.global_state.emit_signal(RenderRootSignal::Minimize);
        }

        /// Exits the application.
        pub fn exit(&mut self) {
            trace!("exit");
            self.global_state.emit_signal(RenderRootSignal::Exit);
        }

        /// Shows the window menu at a specified position.
        pub fn show_window_menu(&mut self, position: LogicalPosition<f64>) {
            trace!("show_window_menu");
            self.global_state
                .emit_signal(RenderRootSignal::ShowWindowMenu(position));
        }

        /// Creates a new [layer] at a specified `position`.
        ///
        /// The given `position` must be in the window's coordinate space.
        ///
        /// # Panics
        ///
        /// If [`W::as_layer()`](Widget::as_layer) returns `None`.
        ///
        /// [layer]: crate::doc::concepts#layers
        pub fn create_layer<W: Widget + ?Sized>(
            &mut self,
            layer_type: LayerType,
            mut fallback_widget: NewWidget<W>,
            position: Point,
        ) {
            trace!("create_layer");

            if fallback_widget.widget.as_layer().is_none() {
                debug_panic!(
                    "cannot create layer of type {} - `Widget::as_layer()` returned None",
                    fallback_widget.widget.short_type_name()
                );
                return;
            }

            self.global_state.emit_signal(RenderRootSignal::NewLayer(
                layer_type,
                fallback_widget.erased(),
                position,
            ));
        }

        /// Creates a new [layer] at a specified `position`, and ties it to the current widget.
        ///
        /// The layer will be removed when the current widget is removed from the tree.
        /// A widget can only have one layer of any given type `W` at a time:
        /// if a widget creates two layers of the same type, the old layer will be removed.
        ///
        /// The given `position` must be in the window's coordinate space.
        ///
        /// # Panics
        ///
        /// If [`W::as_layer()`](Widget::as_layer) returns `None`.
        ///
        /// [layer]: crate::doc::concepts#layers
        pub fn create_attached_layer<W: Widget + ?Sized>(
            &mut self,
            layer_type: LayerType,
            mut fallback_widget: NewWidget<W>,
            position: Point,
        ) {
            trace!("create_attached_layer");

            if fallback_widget.widget.as_layer().is_none() {
                debug_panic!(
                    "cannot create layer of type {} - `Widget::as_layer()` returned None",
                    fallback_widget.widget.short_type_name()
                );
                return;
            }

            // FIXME - Add LayerId type and use that instead.
            let layer_id = fallback_widget.id;

            let layers = self
                .global_state
                .attached_layers
                .entry(self.widget_id())
                .or_default();
            if let Some(prev) = layers.insert(TypeId::of::<W>(), layer_id) {
                self.global_state
                    .emit_signal(RenderRootSignal::RemoveLayer(prev));
            }
            self.global_state.emit_signal(RenderRootSignal::NewLayer(
                layer_type,
                fallback_widget.erased(),
                position,
            ));
        }

        /// Returns the attached layer created by this widget with the given type `W`.
        ///
        /// See [`Self::create_attached_layer`] for more details.
        pub fn get_attached_layer<W: Widget + ?Sized>(&self) -> Option<WidgetId> {
            self.global_state
                .attached_layers
                .get(&self.widget_id())?
                .get(&TypeId::of::<W>())
                .copied()
        }

        /// Removes the layer with the specified widget as root.
        pub fn remove_layer(&mut self, root_widget_id: WidgetId) {
            trace!("remove_layer");
            self.global_state
                .emit_signal(RenderRootSignal::RemoveLayer(root_widget_id));

            let Some(layers) = self.global_state.attached_layers.get_mut(&self.widget_id()) else {
                return;
            };

            let layer_type_id = layers
                .iter()
                .find(|(_, id)| **id == root_widget_id)
                .map(|(type_id, _)| *type_id);
            if let Some(type_id) = layer_type_id {
                layers.remove(&type_id);
            }
        }

        /// Repositions the layer with the specified widget as root.
        ///
        /// The given `position` must be in the window's coordinate space.
        pub fn reposition_layer(&mut self, root_widget_id: WidgetId, position: Point) {
            trace!("reposition_layer");
            self.global_state
                .emit_signal(RenderRootSignal::RepositionLayer(root_widget_id, position));
        }
    }
);

impl UpdateCtx<'_> {
    /// Returns the nearest ancestor of this widget that is of type `W`, along with its `WidgetId`.
    ///
    /// This should only be called during [`WidgetAdded`](crate::core::Update::WidgetAdded).
    /// Calling it in other contexts will always return `None`.
    // FIXME: We should move this method out of `UpdateCtx` to avoid having this disclaimer.
    pub fn nearest_ancestor<W: Widget>(&self) -> Option<(&W, WidgetId)> {
        let mut list = self.ancestors;
        while let Some(node) = list {
            if let Some(widget) = W::from_dyn(node.widget) {
                return Some((widget, node.id));
            }
            list = node.parent;
        }
        None
    }
}

impl RegisterCtx<'_> {
    /// Register a direct layer root and enable its pointer interception.
    /// Nested layer stacks use the same accepted geometry and input routing.
    /// Ordinary children inside a layer keep using [`Self::register_child`].
    pub fn register_layer(&mut self, child: &mut WidgetPod<impl Widget + ?Sized>) {
        self.register_child(child);
        let id = child.id();
        let layer = self.children.item_mut(id).expect("registered layer root");
        if layer.item.widget.as_layer().is_some() && !self.global_state.input_layers.contains(&id) {
            self.global_state.input_layers.push(id);
            self.global_state.input_layer_order_dirty = true;
        }
    }

    /// Registers a child widget.
    ///
    /// Container widgets should call this on all their children in
    /// their implementation of [`Widget::register_children`].
    pub fn register_child(&mut self, child: &mut WidgetPod<impl Widget + ?Sized>) {
        let Some(NewWidget {
            widget,
            id,
            options,
            properties,
            property_stack_id,
            classes,
            tag,
            action_type,
            #[cfg(debug_assertions)]
            action_type_name,
        }) = child.take_inner()
        else {
            return;
        };

        #[cfg(debug_assertions)]
        {
            self.registered_ids.push(id);
        }

        let state = WidgetState::new(
            id,
            widget.short_type_name(),
            options,
            action_type,
            property_stack_id,
            #[cfg(debug_assertions)]
            action_type_name,
        );

        if let Some(tag) = tag {
            let entry = self.global_state.widget_tags.entry(tag);

            let Entry::Vacant(vacant_entry) = entry else {
                debug_panic!("Tag '{tag}' already exists in the widget tree");
                return;
            };

            vacant_entry.insert(id);
        }

        let node = WidgetArenaNode {
            widget: widget.as_box_dyn(),
            state,
            properties,
            class_set: ClassSet {
                classes,
                ..ClassSet::default()
            },
        };
        self.children.insert(id, node);
        self.global_state.registered_widgets += 1;
    }
}

impl Drop for RawCtx<'_> {
    fn drop(&mut self) {
        self.parent_widget_state.merge_up(self.widget_state);
    }
}

// --- MARK: DEBUG PAINT
impl PaintCtx<'_> {
    /// Mark this widget's image brush matrices as local-to-source sampling maps.
    /// Unlike ordinary Painter matrices, these may collapse a source axis.
    pub(crate) fn set_image_sampling_transform(&mut self, sampling: bool) {
        self.widget_state.image_sampling_transform = sampling;
    }

    /// Restrict this widget's image sampling to a source-pixel window.
    /// Cached paints retain the window without changing the shared resource.
    pub fn set_image_source_rect(&mut self, rect: Option<Rect>) {
        self.widget_state.image_source_rect = rect;
    }

    /// Record one image rectangle and retain its actual resource for this draw.
    /// `transform` maps geometry into widget coordinates; `source_transform`
    /// maps that geometry into logical source pixels, including collapsed axes.
    /// The current source window is retained per draw, so tiles can translate
    /// a shared original-image crop into each resource's own pixel coordinates.
    /// The frame snapshot must also admit the resource. Other draws are unchanged.
    pub fn draw_resource_image<S: crate::imaging::PaintSink + ?Sized>(
        &mut self,
        painter: &mut crate::imaging::Painter<'_, S>,
        resource: &crate::resources::ResourceLease,
        bounds: Rect,
        transform: Affine,
        source_transform: Affine,
        sampler: peniko::ImageSampler,
    ) {
        self.image_draws
            .push(crate::core::recorded_paint::ResourceDraw {
                command: self.command_cursor.get(),
                resource: resource.clone(),
                sampler,
                source_rect: self.widget_state.image_source_rect,
                sprites: None,
            });
        painter
            .fill(bounds, peniko::Color::WHITE)
            .transform(transform)
            .brush_transform(Some(source_transform))
            .draw();
    }

    /// Record centered sprites with one shared image, sampling state and lease.
    /// The rectangle supplies source pixels and aspect ratio; the transform maps
    /// batch-local positions to widget coordinates. Failed payload admission is
    /// reported by the frame builder at this command, preserving the accepted frame.
    pub fn draw_resource_sprites<S: crate::imaging::PaintSink + ?Sized>(
        &mut self,
        painter: &mut crate::imaging::Painter<'_, S>,
        resource: &crate::resources::ResourceLease,
        bounds: Rect,
        transform: Affine,
        sampler: peniko::ImageSampler,
        instances: Result<crate::frame::SpriteInstances, crate::frame::BuildErrorKind>,
    ) {
        if instances
            .as_ref()
            .is_ok_and(|values| values.as_slice().is_empty())
        {
            return;
        }
        self.image_draws
            .push(crate::core::recorded_paint::ResourceDraw {
                command: self.command_cursor.get(),
                resource: resource.clone(),
                sampler,
                source_rect: self.widget_state.image_source_rect,
                sprites: Some(instances),
            });
        painter
            .fill(bounds, peniko::Color::WHITE)
            .transform(transform)
            .brush_transform(Some(Affine::IDENTITY))
            .draw();
    }

    /// Multiply image brushes emitted by this widget by a linear premultiplied color.
    /// The value is retained with cached paints and does not affect children or masks.
    pub fn set_image_color(&mut self, color: peniko::Color) {
        self.widget_state.image_color = color
            .convert::<peniko::color::LinearSrgb>()
            .premultiply()
            .components;
    }

    /// Bilinear tint and sRGB adjustment for this widget's image brushes.
    /// Cached paints retain the value; children keep their own appearance.
    /// Native alpha with identity adjustment and no corner colors uses the ordinary draw path.
    pub fn set_image_appearance(
        &mut self,
        colors: Option<[peniko::Color; 4]>,
        adjustment: crate::frame::ImageColorAdjustment,
        alpha_layout: crate::frame::ImageAlphaLayout,
        bounds: Rect,
    ) {
        let gradient = colors.map(|colors| crate::frame::ImageColorGradient {
            colors: colors.map(|color| {
                color
                    .convert::<peniko::color::LinearSrgb>()
                    .premultiply()
                    .components
            }),
            transform: Affine::translate(bounds.origin().to_vec2())
                * Affine::scale_non_uniform(bounds.width(), bounds.height()),
        });
        self.widget_state.image_appearance =
            crate::frame::ImageAppearance::new(gradient, adjustment, alpha_layout);
    }

    /// Controls how this widget subtree is recorded in the current paint pass.
    ///
    /// The declaration is retained across cached pre/paint/post callbacks.
    /// Initial mode is Inline; explicitly set Inline to return to ordinary recording.
    pub fn set_paint_layer_mode(&mut self, mode: PaintLayerMode) {
        self.widget_state.paint_layer_mode = mode;
    }

    /// Declare external content and retain its opaque host token across cached paints.
    pub fn set_external_paint(&mut self, token: u64) {
        self.widget_state.paint_layer_mode = PaintLayerMode::External;
        self.widget_state.external_paint_token = token;
    }

    /// Apply compositing to this widget's pre-paint, paint, children and post-paint.
    ///
    /// A declaration made during `pre_paint` affects the current frame and remains
    /// active across cached paints. Identity values remove the group. Compositing
    /// changes only rendered pixels, leaving layout and input geometry intact.
    pub fn set_subtree_composite(
        &mut self,
        opacity: f32,
        invert: f32,
        blend: crate::frame::BlendMode,
        mask: Option<PaintImageMask>,
    ) {
        self.widget_state.subtree_opacity = opacity;
        self.widget_state.subtree_invert = invert;
        self.widget_state.subtree_blend = blend;
        match (&mut self.widget_state.subtree_image_mask, mask) {
            (Some(current), Some(mask)) => **current = mask,
            (slot, mask) => *slot = mask.map(Box::new),
        }
    }

    /// Process the completed subtree before its opacity, mask and blend.
    /// The prepared declaration persists across cached paints.
    pub fn set_subtree_effect(&mut self, effect: Option<crate::core::PaintEffect>) {
        match (&mut self.widget_state.subtree_effect, effect) {
            (Some(current), Some(effect)) => **current = effect,
            (slot, effect) => *slot = effect.map(Box::new),
        }
    }

    /// Apply scalar shader coverage after the subtree's color effect.
    /// The prepared declaration and image leases persist across cached paints.
    pub fn set_subtree_mask_effect(&mut self, effect: Option<crate::core::PaintEffect>) {
        match (&mut self.widget_state.subtree_mask_effect, effect) {
            (Some(current), Some(effect)) => **current = effect,
            (slot, effect) => *slot = effect.map(Box::new),
        }
    }

    /// Whether debug paint is enabled.
    ///
    /// If this property is set, your widget may draw additional debug information
    /// (such as the position of the text baseline).
    /// These should normally use the [debug color][Self::debug_color] for this widget.
    /// Please note that when debug painting is enabled, each widget's layout boundaries are
    /// outlined by Masonry, so you should avoid duplicating that.
    ///
    /// Debug paint can be enabled by setting the environment variable `MASONRY_DEBUG_PAINT`.
    pub fn debug_paint_enabled(&self) -> bool {
        self.global_state.debug_paint
    }

    /// A color used for debug painting in this widget.
    ///
    /// This is normally used to paint additional debugging information
    /// when debug paint is enabled, see [`Self::debug_paint_enabled`].
    pub fn debug_color(&self) -> Color {
        get_debug_color(self.widget_id().to_raw())
    }
}

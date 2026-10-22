//! Layer ownership and placement for the same authored stage tree.
use super::*;
use crate::stage_widget::popup::PopupSource;
use reel_ui::{
    kurbo::Affine,
    stage::{NodeId, StageIdentity},
    widgets::Popup,
};

pub(super) struct LayerRecord {
    pub node: NodeId,
    pub stage: WidgetId,
    pub layer: WidgetId,
    owner: OwnerHandle,
    source: PopupSource,
    identity: Option<StageIdentity>,
    transform: Affine,
    position: Point,
}
impl SessionUi {
    pub(super) fn stage_for_node(
        bridge: &PresentationBridge,
        root: WidgetId,
        node: NodeId,
    ) -> Option<WidgetId> {
        let widget = bridge
            .root()
            .get_widget(root)?
            .downcast::<SessionWidget>()?;
        let base = widget.stage;
        let contains = |id| {
            bridge
                .root()
                .get_widget(id)
                .and_then(|w| w.downcast::<StageWidget>())
                .is_some_and(|s| s.node_widget(node).is_some())
        };
        if contains(base) {
            Some(base)
        } else {
            widget
                .popup_layers
                .iter()
                .find_map(|layer| contains(layer.stage).then_some(layer.stage))
        }
    }
    pub(super) fn projected_node(
        bridge: &PresentationBridge,
        root: WidgetId,
        node: NodeId,
    ) -> Option<WidgetId> {
        bridge
            .root()
            .get_widget(Self::stage_for_node(bridge, root, node)?)?
            .downcast::<StageWidget>()?
            .node_widget(node)
    }
    pub(super) fn physical_stages(
        bridge: &PresentationBridge,
        root: WidgetId,
    ) -> impl Iterator<Item = WidgetId> + '_ {
        let widget = bridge
            .root()
            .get_widget(root)
            .and_then(|w| w.downcast::<SessionWidget>())
            .map(|w| w.inner());
        widget
            .into_iter()
            .flat_map(|w| std::iter::once(w.stage).chain(w.popup_layers.iter().map(|l| l.stage)))
    }
    pub(super) fn virtual_popup_images_ready(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
        view: &StageView,
        geometry: &reel_ui::app::PresentedGeometryView,
    ) -> bool {
        let mut stages = Self::physical_stages(bridge, root);
        let ready = |id, candidate: &StageView| {
            bridge
                .root()
                .get_widget(id)
                .and_then(|w| w.downcast::<StageWidget>())
                .is_some_and(|widget| {
                    widget.virtual_images_visible_ready(
                        bridge.root(),
                        candidate,
                        geometry,
                        &self.images.loaded,
                    )
                })
        };
        if !stages.next().is_some_and(|id| ready(id, view)) {
            return false;
        }
        for popup in view.popups() {
            if !stages.next().is_some_and(|id| ready(id, &popup.view(view))) {
                return false;
            }
        }
        stages.next().is_none()
    }
    pub(super) fn sync_popups(&self, bridge: &mut PresentationBridge, root: WidgetId) {
        self.sync_popup_view(bridge, root, self.stage.as_ref())
    }
    pub(super) fn sync_popup_view(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        view: Option<&StageView>,
    ) {
        let sources = view.map_or(&[][..], StageView::popups);
        let identity = view.and_then(|view| view.identity.as_ref());
        let widget = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        if sources.is_empty() && widget.popup_layers.is_empty() {
            return;
        }
        if widget.source_owner == self.source_owner
            && sources.len() == widget.popup_layers.len()
            && sources
                .iter()
                .zip(&widget.popup_layers)
                .all(|(source, layer)| {
                    layer.identity.as_ref() == identity && source.same_subtree(&layer.source)
                })
        {
            self.position_popups(bridge, root);
            return;
        }
        let content = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap()
            .content
            .id();
        let mut layers = bridge.edit_widget(root, |mut widget| {
            let widget = widget.downcast::<SessionWidget>();
            std::mem::take(&mut widget.widget.popup_layers)
        });
        let identity = identity.cloned();
        for index in (0..layers.len()).rev() {
            if layers[index].owner != self.source_owner
                || layers[index].identity != identity
                || !sources.iter().any(|s| s.id() == layers[index].node)
            {
                let id = layers.remove(index).layer;
                bridge.edit_widget(content, |mut stack| {
                    LayerStack::remove_layer(&mut stack.downcast::<LayerStack>(), id);
                });
            }
        }
        for (index, source) in sources.iter().enumerate() {
            if layers.get(index).is_none_or(|l| l.node != source.id()) {
                if let Some(old) = layers.iter().position(|l| l.node == source.id()) {
                    let retained = layers.remove(old);
                    bridge.edit_widget(content, |mut stack| {
                        LayerStack::move_layer(
                            &mut stack.downcast::<LayerStack>(),
                            retained.layer,
                            index + 1,
                        );
                    });
                    layers.insert(index, retained);
                } else {
                    let view = source.view(view.unwrap());
                    let stage = NewWidget::new(StageWidget::new_portal(&view));
                    let stage_id = stage.id();
                    let layer =
                        NewWidget::new(Popup::new(stage).with_modal(source.properties().modal));
                    let layer_id = layer.id();
                    bridge.edit_widget(content, |mut stack| {
                        LayerStack::add_layer(
                            &mut stack.downcast::<LayerStack>(),
                            layer,
                            Point::ORIGIN,
                        );
                    });
                    if index < layers.len() {
                        bridge.edit_widget(content, |mut stack| {
                            LayerStack::move_layer(
                                &mut stack.downcast::<LayerStack>(),
                                layer_id,
                                index + 1,
                            );
                        });
                    }
                    layers.insert(
                        index,
                        LayerRecord {
                            node: source.id(),
                            stage: stage_id,
                            layer: layer_id,
                            owner: self.source_owner,
                            source: source.clone(),
                            identity: identity.clone(),
                            transform: Affine::IDENTITY,
                            position: Point::ORIGIN,
                        },
                    );
                }
            }
            let layer = &mut layers[index];
            let modal_changed = source.properties().modal != layer.source.properties().modal;
            let view = source.view(view.unwrap());
            if !source.same_subtree(&layer.source) {
                bridge.edit_widget(layer.stage, |mut widget| {
                    StageWidget::sync(&mut widget.downcast::<StageWidget>(), &view);
                });
            }
            if modal_changed {
                bridge.edit_widget(layer.layer, |mut widget| {
                    Popup::set_modal(&mut widget.downcast::<Popup>(), source.properties().modal)
                });
            }
            layer.source = source.clone();
        }
        // Publish the mounted index before querying nested anchors in earlier layers.
        bridge.edit_widget(root, |mut widget| {
            widget.downcast::<SessionWidget>().widget.popup_layers = layers
        });
        self.position_popups(bridge, root)
    }
    fn position_popups(&self, bridge: &mut PresentationBridge, root: WidgetId) {
        let content = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap()
            .content
            .id();
        let content_widget = bridge.root().get_widget(content).unwrap();
        let inverse = content_widget.ctx().window_transform().inverse();
        let viewport = content_widget.ctx().border_box();
        let count = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap()
            .popup_layers
            .len();
        for index in 0..count {
            let (layer, anchor, source, previous, previous_position) = {
                let widget = bridge
                    .root()
                    .get_widget(root)
                    .unwrap()
                    .downcast::<SessionWidget>()
                    .unwrap();
                let record = &widget.popup_layers[index];
                (
                    record.layer,
                    record.source.anchor,
                    record.source.clone(),
                    record.transform,
                    record.position,
                )
            };
            let anchor = Self::projected_node(bridge, root, anchor).unwrap();
            let anchor = {
                let widget = bridge.root().get_widget(anchor).unwrap();
                (inverse * widget.ctx().window_transform())
                    .transform_rect_bbox(widget.ctx().border_box())
            };
            let size = bridge
                .root()
                .get_widget(layer)
                .unwrap()
                .ctx()
                .border_box()
                .size();
            let props = source.properties();
            let offset = Vec2::new(props.x, props.y);
            let transform = Affine::translate(-offset) * props.transform(size);
            let bounds = transform.transform_rect_bbox(size.to_rect());
            let config = source.popup();
            let placed = config
                .placement
                .position(anchor, bounds.size(), viewport, config.offset);
            let position = placed - bounds.origin().to_vec2() + offset;
            if transform != previous {
                bridge.edit_widget(layer, |mut widget| widget.set_transform(transform));
                bridge.edit_widget(root, |mut widget| {
                    widget.downcast::<SessionWidget>().widget.popup_layers[index].transform =
                        transform
                });
            }
            if position != previous_position {
                bridge.edit_widget(content, |mut stack| {
                    LayerStack::reposition_layer(
                        &mut stack.downcast::<LayerStack>(),
                        layer,
                        position,
                    );
                });
                bridge.edit_widget(root, |mut widget| {
                    widget.downcast::<SessionWidget>().widget.popup_layers[index].position =
                        position;
                });
            }
        }
    }
}

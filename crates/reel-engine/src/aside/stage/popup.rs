//! A floating subtree keeps its ordinary logical parent as its anchor and owner.
use super::*;
use reel_ui::{stage::PopupView, widgets::PopupPlacement};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum PopupField {
    /// Preferred side and alignment: bottom-start, bottom-end, top-start, top-end,
    /// left-start, left-end, right-start or right-end. Placement flips to fit.
    #[aside(name = "placement", ty = String, default = "bottom-start", validate = validate_placement)]
    Placement,
    /// Gap from the parent's presented bounds, in logical canvas pixels.
    #[aside(name = "offset", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Offset,
    /// Receives {target, reason}, with reason outside or back. Removing target closes
    /// the popup; callbacks may instead keep it open. Selecting an item is separate.
    #[aside(name = "on_dismiss", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnDismiss,
}

fn placement(heap: &Heap, value: Value) -> Result<PopupPlacement, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 12 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 12];
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    Ok(match &bytes[..info.len as usize] {
        b"bottom-start" => PopupPlacement::BottomStart,
        b"bottom-end" => PopupPlacement::BottomEnd,
        b"top-start" => PopupPlacement::TopStart,
        b"top-end" => PopupPlacement::TopEnd,
        b"left-start" => PopupPlacement::LeftStart,
        b"left-end" => PopupPlacement::LeftEnd,
        b"right-start" => PopupPlacement::RightStart,
        b"right-end" => PopupPlacement::RightEnd,
        _ => return Err(HeapError::TypeMismatch),
    })
}
fn validate_placement(heap: &Heap, value: Value) -> Result<(), HeapError> {
    placement(heap, value).map(|_| ())
}
pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = PopupField::PROPERTIES.len() as u32;
    if index < prefix {
        PopupField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}
pub(super) fn kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let get = |field: PopupField| heap.table_entry(fields, field.index() as u32).map(|p| p.1);
    let handler = get(PopupField::OnDismiss)?;
    validate_callback(heap, handler)?;
    Ok(NodeKind::Popup(PopupView {
        placement: placement(heap, get(PopupField::Placement)?)?,
        offset: number(get(PopupField::Offset)?)?,
        listening: handler.tag() != ValueTag::Null,
    }))
}

impl StageBinding {
    /// Floating layers paint after the base scene; nested layers follow DFS order.
    pub(super) fn popup_modal_scope(&self, heap: &Heap) -> Result<Option<NodeId>, AdapterError> {
        let mut pending: Vec<_> = self
            .scene
            .root()
            .map(|id| (id, None, true))
            .into_iter()
            .collect();
        let mut layer = 0;
        let mut selected = None;
        while let Some((id, mut current_layer, ancestors_enabled)) = pending.pop() {
            let node = self.scene.node(id).unwrap();
            if matches!(node.kind, NodeKind::Cursor) {
                continue;
            }
            let host = self.value(heap, id)?;
            let fields = heap.host_fields(host)?;
            let offset = self.property_offset(heap, host)? as usize;
            let selection = overrides::selection(heap, host)?;
            let flag = |property: NodeProperty| -> Result<bool, AdapterError> {
                Ok(
                    overrides::get(heap, fields, offset, selection, property.index())?.as_bool()
                        == Some(true),
                )
            };
            if !flag(NodeProperty::Visible)? {
                continue;
            }
            let enabled = ancestors_enabled && flag(NodeProperty::Enabled)?;
            if matches!(node.kind, NodeKind::Popup(_)) {
                if node.parent.is_none() || !enabled {
                    continue;
                }
                layer += 1;
                current_layer = Some(layer);
            }
            if let Some(current) = current_layer
                && flag(NodeProperty::Modal)?
                && selected.is_none_or(|(previous, _)| current >= previous)
            {
                selected = Some((current, id));
            }
            pending.extend(
                node.children
                    .iter()
                    .rev()
                    .map(|&id| (id, current_layer, enabled)),
            );
        }
        Ok(selected.map(|(_, id)| id))
    }

    pub(in crate::aside) fn dismiss_popup(
        &self,
        heap: &Heap,
        id: NodeId,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(id) else {
            return Ok(None);
        };
        if !matches!(node.kind, NodeKind::Popup(_)) {
            return Ok(None);
        }
        let target = self.value(heap, id)?;
        let callback = heap
            .table_entry(
                heap.host_fields(target)?,
                PopupField::OnDismiss.index() as u32,
            )?
            .1;
        if callback.tag() == ValueTag::Null {
            return Ok(None);
        }
        Ok(self
            .mounted_in_scope(heap, id, self.modal_scope(heap)?)?
            .then_some((target, callback)))
    }
}

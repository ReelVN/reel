//! Authored offsets and measured scroll geometry share the normal property graph.
use super::*;
use reel_ui::{
    kurbo::Vec2,
    stage::NodeScroll,
    widgets::{ScrollAlign, ScrollAxis},
};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum ScrollField {
    #[aside(name = "axis", ty = String, default = "vertical", validate = validate_axis)]
    Axis,
    #[aside(name = "follow_end", ty = Bool, default = false)]
    FollowEnd,
    #[aside(name = "content_align", ty = String, default = "start", validate = validate_align)]
    ContentAlign,
    #[aside(name = "scroll_x", ty = Any, default = 0, validate = validate_number)]
    X,
    #[aside(name = "scroll_y", ty = Any, default = 0, validate = validate_number)]
    Y,
}
pub(super) const METRICS: [&str; 4] = [
    "viewport_width",
    "viewport_height",
    "content_width",
    "content_height",
];
fn axis(heap: &Heap, value: Value) -> Result<ScrollAxis, HeapError> {
    word(heap, value, |word| match word {
        b"horizontal" => Ok(ScrollAxis::Horizontal),
        b"vertical" => Ok(ScrollAxis::Vertical),
        b"both" => Ok(ScrollAxis::Both),
        _ => Err(HeapError::TypeMismatch),
    })
}
fn word<T>(
    heap: &Heap,
    value: Value,
    parse: impl FnOnce(&[u8]) -> Result<T, HeapError>,
) -> Result<T, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 10 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 10];
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    parse(&bytes[..info.len as usize])
}
fn align(heap: &Heap, value: Value) -> Result<ScrollAlign, HeapError> {
    word(heap, value, |word| match word {
        b"start" => Ok(ScrollAlign::Start),
        b"center" => Ok(ScrollAlign::Center),
        b"end" => Ok(ScrollAlign::End),
        _ => Err(HeapError::TypeMismatch),
    })
}
fn validate_axis(heap: &Heap, value: Value) -> Result<(), HeapError> {
    axis(heap, value).map(|_| ())
}
fn validate_align(heap: &Heap, value: Value) -> Result<(), HeapError> {
    align(heap, value).map(|_| ())
}
pub(super) fn kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let get =
        |field: ScrollField| Ok::<_, HeapError>(heap.table_entry(fields, field.index() as u32)?.1);
    Ok(NodeKind::Scroll {
        axis: axis(heap, get(ScrollField::Axis)?)?,
        follow_end: boolean(get(ScrollField::FollowEnd)?)?,
        align: align(heap, get(ScrollField::ContentAlign)?)?,
        offset: Vec2::new(number(get(ScrollField::X)?)?, number(get(ScrollField::Y)?)?),
    })
}
pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = ScrollField::PROPERTIES.len() as u32;
    let metrics = prefix + NodeProperty::PROPERTIES.len() as u32 + interaction::FIELDS.len() as u32;
    if index < prefix {
        ScrollField::validate(heap, index, value)
    } else if (metrics..metrics + METRICS.len() as u32).contains(&index) {
        validate_nonnegative(heap, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}
impl StageBinding {
    pub(in crate::aside) fn scroll(
        &mut self,
        heap: &mut Heap,
        event: &NodeScroll,
    ) -> Result<bool, AdapterError> {
        let Some(binding) = self.handles.get(&event.node) else {
            return Ok(false);
        };
        if !matches!(
            self.scene.node(event.node).map(|n| &n.kind),
            Some(NodeKind::Scroll { .. })
        ) {
            return Ok(false);
        }
        let host = heap.import_handle(binding.handle)?;
        let offset_fields = [ScrollField::X.index() as u32, ScrollField::Y.index() as u32];
        let mut changed = false;
        for ((field, requested), actual) in offset_fields
            .into_iter()
            .zip([event.requested.x, event.requested.y])
            .zip([event.offset.x, event.offset.y])
        {
            let property = PropertyId { host, field };
            let current = number(heap.host_property(property)?)?;
            // A newer script write wins over feedback from the previous projection.
            if current == requested && current != actual {
                heap.write_host_property(property, Value::float(actual), true)?;
                changed = true;
            }
        }
        let metrics = binding.interaction_offset + interaction::FIELDS.len() as u32;
        for (i, value) in [
            event.viewport.width,
            event.viewport.height,
            event.content.width,
            event.content.height,
        ]
        .into_iter()
        .enumerate()
        {
            let property = PropertyId {
                host,
                field: metrics + i as u32,
            };
            if number(heap.host_property(property)?)? != value {
                heap.publish_host_property(property, Value::float(value))?;
                changed = true;
            }
        }
        Ok(changed)
    }
}

//! Authored offsets and measured scroll geometry share the normal property graph.
use super::*;
use reel_ui::{
    kurbo::Vec2,
    stage::NodeScroll,
    widgets::{ScrollAlign, ScrollAxis, ScrollOverscroll},
};
mod reveal;
pub(in crate::aside) use reveal::documentation;

#[derive(aside_host::Properties)]
pub(in crate::aside) enum ScrollField {
    #[aside(name = "axis", ty = String, default = "vertical", validate = validate_axis)]
    Axis,
    /// Boundary behavior: `clamp` keeps direct touch panning bounded; `bounce`
    /// adds a damped edge pull and native return animation. Logical offsets stay
    /// bounded in both modes. Wheel and programmatic positioning remain clamped.
    #[aside(name = "overscroll", ty = String, default = "clamp", validate = validate_overscroll)]
    Overscroll,
    #[aside(name = "follow_end", ty = Bool, default = false)]
    FollowEnd,
    #[aside(name = "content_align", ty = String, default = "start", validate = validate_align)]
    ContentAlign,
    /// Logical horizontal offset. Layout feedback preserves an owning activity;
    /// user scrolling or an explicit assignment takes over that activity.
    #[aside(name = "scroll_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    X,
    /// Logical vertical offset. Layout feedback preserves an owning activity;
    /// user scrolling or an explicit assignment takes over that activity.
    #[aside(name = "scroll_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Y,
    /// Vertical spacing between user scroll stops in canvas units.
    #[aside(name = "snap_y", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, readonly, validate = validate_snap)]
    SnapY,
}
pub(super) fn snap(value: Value) -> Result<Option<f64>, AdapterError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    let step = number(value)?;
    if step <= 0. {
        return Err(AdapterError::Input("scroll snap_y must be positive"));
    }
    Ok(Some(step))
}
fn validate_snap(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(snap(value))
}
pub(super) const METRICS: [(&str, &str); 6] = [
    (
        "viewport_width",
        "The accepted viewport width in logical pixels.",
    ),
    (
        "viewport_height",
        "The accepted viewport height in logical pixels.",
    ),
    (
        "content_width",
        "The accepted content width in logical pixels.",
    ),
    (
        "content_height",
        "The accepted content height in logical pixels.",
    ),
    (
        "max_scroll_x",
        "The accepted rightmost scroll offset in logical pixels. Zero when horizontal scrolling is unavailable.",
    ),
    (
        "max_scroll_y",
        "The accepted bottom scroll offset in logical pixels. Zero when vertical scrolling is unavailable.",
    ),
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
pub(in crate::aside) const OVERSCROLL_CHOICES: [(&str, ScrollOverscroll); 2] = [
    ("clamp", ScrollOverscroll::Clamp),
    ("bounce", ScrollOverscroll::Bounce),
];

fn overscroll(heap: &Heap, value: Value) -> Result<ScrollOverscroll, HeapError> {
    word(heap, value, |word| {
        OVERSCROLL_CHOICES
            .into_iter()
            .find_map(|(name, mode)| (name.as_bytes() == word).then_some(mode))
            .ok_or(HeapError::TypeMismatch)
    })
}
fn validate_overscroll(heap: &Heap, value: Value) -> Result<(), HeapError> {
    overscroll(heap, value).map(|_| ())
}
fn validate_align(heap: &Heap, value: Value) -> Result<(), HeapError> {
    align(heap, value).map(|_| ())
}
pub(super) fn kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let get =
        |field: ScrollField| Ok::<_, HeapError>(heap.table_entry(fields, field.index() as u32)?.1);
    Ok(NodeKind::Scroll {
        axis: axis(heap, get(ScrollField::Axis)?)?,
        overscroll: overscroll(heap, get(ScrollField::Overscroll)?)?,
        follow_end: boolean(get(ScrollField::FollowEnd)?)?,
        align: align(heap, get(ScrollField::ContentAlign)?)?,
        offset: Vec2::new(number(get(ScrollField::X)?)?, number(get(ScrollField::Y)?)?),
        snap_y: snap(get(ScrollField::SnapY)?)?,
    })
}
pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = ScrollField::PROPERTIES.len() as u32;
    let metrics =
        prefix + NodeProperty::PROPERTIES.len() as u32 + interaction::Field::NAMES.len() as u32;
    if index < prefix {
        ScrollField::validate(heap, index, value)
    } else if (metrics..metrics + METRICS.len() as u32).contains(&index) {
        validate_nonnegative(heap, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}
impl StageBinding {
    pub(super) fn initialize_scroll_metrics(
        &mut self,
        heap: &mut Heap,
        host: Value,
        fields: Value,
        first: u32,
    ) -> Result<(), HeapError> {
        for (index, (name, _)) in METRICS.into_iter().enumerate() {
            let key = self.property_key(heap, name)?;
            heap.table_store(fields, first + index as u32, key, Value::float(0.))?;
        }
        heap.observe_host_properties(host, first, METRICS.len() as u32)?;
        Ok(())
    }
    pub(in crate::aside) fn scroll(
        &mut self,
        heap: &mut Heap,
        event: &NodeScroll,
    ) -> Result<(bool, bool), AdapterError> {
        let Some(binding) = self.handles.get(&event.node) else {
            return Ok((false, false));
        };
        let Some(NodeKind::Scroll { axis, .. }) = self.scene.node(event.node).map(|n| &n.kind)
        else {
            return Ok((false, false));
        };
        let maximum = axis.maximum(event.viewport, event.content);
        let host = heap.import_handle(binding.handle)?;
        let offset_fields = [ScrollField::X.index() as u32, ScrollField::Y.index() as u32];
        let mut changed = false;
        let mut accepted_y = false;
        for ((field, requested), actual) in offset_fields
            .into_iter()
            .zip([event.requested.x, event.requested.y])
            .zip([event.offset.x, event.offset.y])
        {
            let property = PropertyId { host, field };
            let current = number(heap.host_property(property)?)?;
            if field == ScrollField::Y.index() as u32 {
                accepted_y = event.input || current == requested;
            }
            // A newer script write wins over feedback from the previous projection.
            if (event.input || current == requested) && (event.input || current != actual) {
                heap.write_host_property(property, Value::float(actual), event.input)?;
                changed = true;
            }
        }
        let metrics = binding.interaction_offset + interaction::Field::NAMES.len() as u32;
        for (i, value) in [
            event.viewport.width,
            event.viewport.height,
            event.content.width,
            event.content.height,
            maximum.x,
            maximum.y,
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
        Ok((changed, accepted_y))
    }
}

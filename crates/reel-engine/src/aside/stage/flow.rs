//! Typed layout declarations shared by runtime validation and editor tooling.
use super::*;
use reel_ui::stage::{FlowOptions, GridOptions, Justify};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum FlowField {
    /// Wrap children onto additional lines when the flow axis is constrained.
    #[aside(name = "wrap", ty = Bool, default = false)]
    Wrap,
    /// Distribute remaining flow space: start, center, end, space_between, space_around or space_evenly.
    #[aside(name = "justify", ty = String, default = "start", validate = validate_justify)]
    Justify,
    /// Space between wrapped lines; null inherits gap.
    #[aside(name = "line_gap", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    LineGap,
}
#[derive(aside_host::Properties)]
pub(in crate::aside) enum GridField {
    /// Number of equal-width columns, from 1 to 10000. Children fill rows in source order.
    #[aside(name = "columns", ty = Int, default = 1, validate = validate_columns)]
    Columns,
    /// Fixed width for each column; null shares the available width equally.
    #[aside(name = "column_width", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    ColumnWidth,
    /// Fixed row height; null measures each row from its children.
    #[aside(name = "row_height", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    RowHeight,
    /// Horizontal space between columns; null inherits gap.
    #[aside(name = "column_gap", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    ColumnGap,
    /// Vertical space between rows; null inherits gap.
    #[aside(name = "row_gap", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    RowGap,
}
fn justify(heap: &Heap, value: Value) -> Result<Justify, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 13 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 13];
    heap.read_bytes(value, 0, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"start" => Ok(Justify::Start),
        b"center" => Ok(Justify::Center),
        b"end" => Ok(Justify::End),
        b"space_between" => Ok(Justify::SpaceBetween),
        b"space_around" => Ok(Justify::SpaceAround),
        b"space_evenly" => Ok(Justify::SpaceEvenly),
        _ => Err(HeapError::TypeMismatch),
    }
}
fn validate_justify(heap: &Heap, value: Value) -> Result<(), HeapError> {
    justify(heap, value).map(|_| ())
}
fn validate_columns(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.as_int().is_some_and(|v| (1..=10000).contains(&v)) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
fn optional(value: Value) -> Result<Option<f32>, AdapterError> {
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        Ok(Some(nonnegative(value)? as f32))
    }
}
pub(super) fn flow_kind(heap: &Heap, fields: Value, row: bool) -> Result<NodeKind, AdapterError> {
    let get =
        |field: FlowField| Ok::<_, HeapError>(heap.table_entry(fields, field.index() as u32)?.1);
    let options = FlowOptions {
        wrap: boolean(get(FlowField::Wrap)?)?,
        justify: justify(heap, get(FlowField::Justify)?)?,
        line_gap: optional(get(FlowField::LineGap)?)?,
    };
    Ok(if row {
        NodeKind::Row(options)
    } else {
        NodeKind::Column(options)
    })
}
pub(super) fn grid_kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let get =
        |field: GridField| Ok::<_, HeapError>(heap.table_entry(fields, field.index() as u32)?.1);
    let columns = get(GridField::Columns)?;
    validate_columns(heap, columns)?;
    Ok(NodeKind::Grid(GridOptions {
        columns: columns.as_int().unwrap() as u16,
        column_width: optional(get(GridField::ColumnWidth)?)?,
        row_height: optional(get(GridField::RowHeight)?)?,
        column_gap: optional(get(GridField::ColumnGap)?)?,
        row_gap: optional(get(GridField::RowGap)?)?,
    }))
}
pub(super) fn validate_flow(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    validate::<FlowField>(heap, index, value)
}
pub(super) fn validate_grid(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    validate::<GridField>(heap, index, value)
}
fn validate<P: PropertySchema>(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = P::PROPERTIES.len() as u32;
    if index < prefix {
        P::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}

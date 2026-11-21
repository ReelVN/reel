//! Camera properties use the normal host property graph and action runner.
use super::*;
use reel_ui::stage::CameraView;

#[derive(aside_host::Properties)]
pub(in crate::aside) enum CameraField {
    #[aside(name = "camera_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    X,
    #[aside(name = "camera_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Y,
    #[aside(name = "zoom", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_zoom, animatable)]
    Zoom,
    #[aside(name = "camera_rotation", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Rotation,
    /// Move the camera toward positive plane distances.
    #[aside(name = "camera_z", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Z,
    /// Perspective of a plane at this distance is one before applying zoom.
    #[aside(name = "focal_length", ty = Any, doc_type = aside_host::source::number(), default = 600, validate = validate_zoom, animatable)]
    FocalLength,
}

#[derive(aside_host::Properties)]
pub(in crate::aside) enum PlaneField {
    /// Distance along the camera axis before moving the camera.
    #[aside(name = "distance", ty = Any, doc_type = aside_host::source::number(), default = 600, validate = validate_number, animatable)]
    Distance,
}
fn zoom(value: Value) -> Result<f64, HeapError> {
    match number(value) {
        Ok(value) if value > 0. => Ok(value),
        _ => Err(HeapError::TypeMismatch),
    }
}
fn validate_zoom(_: &Heap, value: Value) -> Result<(), HeapError> {
    zoom(value).map(|_| ())
}
pub(super) fn kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let get = |field: CameraField| -> Result<Value, HeapError> {
        Ok(heap.table_entry(fields, field.index() as u32)?.1)
    };
    Ok(NodeKind::Camera(CameraView {
        x: number(get(CameraField::X)?)?,
        y: number(get(CameraField::Y)?)?,
        zoom: zoom(get(CameraField::Zoom)?)?,
        rotation: number(get(CameraField::Rotation)?)?,
        z: number(get(CameraField::Z)?)?,
        focal_length: zoom(get(CameraField::FocalLength)?)?,
    }))
}
pub(super) fn plane_kind(heap: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    let distance = number(
        heap.table_entry(fields, PlaneField::Distance.index() as u32)?
            .1,
    )?;
    Ok(NodeKind::CameraPlane { distance })
}

pub(super) fn validate(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = CameraField::PROPERTIES.len() as u32;
    if index < prefix {
        CameraField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}

pub(super) fn validate_plane(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    let prefix = PlaneField::PROPERTIES.len() as u32;
    if index < prefix {
        PlaneField::validate(heap, index, value)
    } else {
        validate_layout_property(heap, index - prefix, value)
    }
}

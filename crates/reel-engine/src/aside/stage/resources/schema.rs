//! Logical identities reuse the ordinary constructors' public property schemas.
use super::*;
use snapshot::Shape;

const SHAPES: &[(Shape, &str, &str)] = &[
    (Shape::Text, "reel.text", "reel::text"),
    (Shape::TextInput, "reel.text_input", "reel::text_input"),
    (Shape::TextArea, "reel.text_area", "reel::text_area"),
    (Shape::Button, "reel.button", "reel::button"),
    (Shape::Slider, "reel.slider", "reel::slider"),
    (
        Shape::ProgressBar,
        "reel.progress_bar",
        "reel::progress_bar",
    ),
    (Shape::Switch, "reel.toggle", "reel::toggle"),
    (Shape::Checkbox, "reel.checkbox", "reel::checkbox"),
    (Shape::Radio, "reel.radio", "reel::radio"),
    (Shape::Input, "reel.input", "reel::input"),
    (Shape::Image, "reel.image", "reel::image"),
    (Shape::Video, "reel.video", "reel::video"),
    (Shape::Camera, "reel.camera", "reel::camera"),
    (Shape::Scroll, "reel.scroll", "reel::scroll"),
    (Shape::Row, "reel.row", "reel::row"),
    (Shape::Column, "reel.column", "reel::column"),
    (Shape::Stack, "reel.stack", "reel::stack"),
    (Shape::Cursor, "reel.cursor", "reel::cursor"),
    (Shape::Grid, "reel.grid", "reel::grid"),
    (Shape::Popup, "reel.popup", "reel::popup"),
    (Shape::Tooltip, "reel.tooltip", "reel::tooltip"),
];
pub(super) fn kind(shape: Shape) -> Option<&'static str> {
    SHAPES
        .iter()
        .find(|entry| entry.0 == shape)
        .map(|entry| entry.1)
}
pub(super) fn shape(kind: &str) -> Option<Shape> {
    SHAPES
        .iter()
        .find(|entry| entry.1 == kind)
        .map(|entry| entry.0)
}
pub(super) fn symbol(shape: Shape) -> &'static str {
    SHAPES.iter().find(|entry| entry.0 == shape).unwrap().2
}
pub(super) fn properties(shape: Shape) -> &'static [aside_host::Property] {
    match shape {
        Shape::Text => appearance::TextField::PROPERTIES,
        Shape::TextInput | Shape::TextArea => text_input::TextInputField::PROPERTIES,
        Shape::Button => appearance::ButtonField::PROPERTIES,
        Shape::Slider => controls::SliderField::PROPERTIES,
        Shape::ProgressBar => controls::ProgressField::PROPERTIES,
        Shape::Switch | Shape::Checkbox => controls::ToggleField::PROPERTIES,
        Shape::Radio => controls::RadioField::PROPERTIES,
        Shape::Input => input::InputField::PROPERTIES,
        Shape::Image | Shape::Video => appearance::ImageField::PROPERTIES,
        Shape::Camera => camera::CameraField::PROPERTIES,
        Shape::Scroll => scroll::ScrollField::PROPERTIES,
        Shape::Popup => popup::PopupField::PROPERTIES,
        Shape::Tooltip => popup::TOOLTIP_PROPERTIES,
        Shape::Row | Shape::Column => flow::FlowField::PROPERTIES,
        Shape::Grid => flow::GridField::PROPERTIES,
        Shape::Stack | Shape::Cursor => &[],
        _ => &[],
    }
}
pub(super) fn writable(shape: Shape) -> impl Iterator<Item = u32> {
    let fields = match shape {
        Shape::Slider => [Some(controls::SliderField::Value.index() as u32), None],
        Shape::Switch | Shape::Checkbox => {
            [Some(controls::ToggleField::Value.index() as u32), None]
        }
        Shape::Radio => [Some(controls::RadioField::Value.index() as u32), None],
        Shape::TextInput | Shape::TextArea => {
            [Some(text_input::TextInputField::Value.index() as u32), None]
        }
        Shape::Scroll => [
            Some(scroll::ScrollField::X.index() as u32),
            Some(scroll::ScrollField::Y.index() as u32),
        ],
        _ => [None, None],
    };
    fields.into_iter().flatten()
}

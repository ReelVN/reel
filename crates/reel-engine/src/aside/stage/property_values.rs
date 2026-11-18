//! Author property decoding and validation.
use super::*;

#[cfg(test)]
#[path = "property_values_tests.rs"]
mod tests;

pub(in crate::aside) fn read_text(
    heap: &Heap,
    value: Value,
    read_limit: usize,
) -> Result<String, AdapterError> {
    text::read_string(heap, value, read_limit)
}

pub(in crate::aside) fn read_source(
    heap: &Heap,
    value: Value,
    read_limit: usize,
) -> Result<reel_ui::stage::ImageSource, AdapterError> {
    if heap.info(value)?.kind == ObjectKind::Bytes {
        let length = heap.info(value)?.len;
        if length == 0 {
            return Err(AdapterError::Input("image source must not be empty"));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        while bytes.len() < length as usize {
            bytes.extend_from_slice(heap.byte_segment(value, bytes.len() as u32)?);
        }
        return Ok(reel_ui::stage::ImageSource::encoded(bytes));
    }
    let source = read_text(heap, value, read_limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("image source must not be empty"));
    }
    Ok(source.into())
}

pub(in crate::aside) const FIT_CHOICES: [(&str, reel_ui::properties::ObjectFit); 3] = [
    ("contain", reel_ui::properties::ObjectFit::Contain),
    ("cover", reel_ui::properties::ObjectFit::Cover),
    ("stretch", reel_ui::properties::ObjectFit::Stretch),
];
pub(in crate::aside) const SAMPLING_CHOICES: [(&str, reel_ui::peniko::ImageQuality); 2] = [
    ("nearest", reel_ui::peniko::ImageQuality::Low),
    ("linear", reel_ui::peniko::ImageQuality::Medium),
];

pub(in crate::aside) fn read_fit(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::properties::ObjectFit, AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        ));
    }
    let mut bytes = [0; 7];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    FIT_CHOICES
        .into_iter()
        .find_map(|(name, fit)| (name.as_bytes() == &bytes[..info.len as usize]).then_some(fit))
        .ok_or(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        ))
}

pub(in crate::aside) fn read_sampling(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::peniko::ImageQuality, AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(AdapterError::Input("sampling must be nearest or linear"));
    }
    let mut bytes = [0; 7];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    SAMPLING_CHOICES
        .into_iter()
        .find_map(|(name, quality)| {
            (name.as_bytes() == &bytes[..info.len as usize]).then_some(quality)
        })
        .ok_or(AdapterError::Input("sampling must be nearest or linear"))
}

pub(in crate::aside) fn read_alpha_layout(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::frame::ImageAlphaLayout, AdapterError> {
    use reel_ui::frame::ImageAlphaLayout;
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 10 {
        return Err(AdapterError::Input(
            "alpha_layout must be native or top_bottom",
        ));
    }
    let mut bytes = [0; 10];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"native" => Ok(ImageAlphaLayout::Native),
        b"top_bottom" => Ok(ImageAlphaLayout::TopBottom),
        _ => Err(AdapterError::Input(
            "alpha_layout must be native or top_bottom",
        )),
    }
}

pub(in crate::aside) fn read_source_rect(
    heap: &Heap,
    value: Value,
) -> Result<Option<reel_ui::kurbo::Rect>, AdapterError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    let invalid = || {
        AdapterError::Input(
            "image source_rect requires null or a finite (x, y, width, height) tuple with positive dimensions and finite float32 endpoints",
        )
    };
    if heap.info(value)?.kind != ObjectKind::Tuple || heap.sequence_len(value)? != 4 {
        return Err(invalid());
    }
    let mut values = [0.; 4];
    for (index, output) in values.iter_mut().enumerate() {
        let value = heap.sequence_get(value, index as u32)?;
        *output = value
            .as_float()
            .or_else(|| value.as_int().map(|n| n as f64))
            .ok_or_else(invalid)?;
        if !(*output as f32).is_finite() {
            return Err(invalid());
        }
    }
    let [x, y, width, height] = values;
    let rect = reel_ui::kurbo::Rect::new(x, y, x + width, y + height);
    if width <= 0. || height <= 0. || !valid_source_rect(rect) {
        return Err(invalid());
    }
    Ok(Some(rect))
}

pub(in crate::aside) fn valid_source_rect(rect: reel_ui::kurbo::Rect) -> bool {
    (rect.width() as f32) > 0.
        && (rect.height() as f32) > 0.
        && [
            rect.x0,
            rect.y0,
            rect.x1,
            rect.y1,
            rect.width(),
            rect.height(),
        ]
        .iter()
        .all(|n| (*n as f32).is_finite())
}

pub(in crate::aside) fn read_slice(
    heap: &Heap,
    value: Value,
) -> Result<Option<reel_ui::kurbo::Insets>, AdapterError> {
    use reel_ui::kurbo::Insets;
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    if matches!(value.tag(), ValueTag::Int | ValueTag::Float) {
        let border = nonnegative(value)?;
        return Ok(Some(Insets::uniform(border)));
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 4
    {
        return Err(AdapterError::Input(
            "image slice requires null, a nonnegative number, or [left, top, right, bottom]",
        ));
    }
    Ok(Some(Insets::new(
        nonnegative(heap.sequence_get(value, 0)?)?,
        nonnegative(heap.sequence_get(value, 1)?)?,
        nonnegative(heap.sequence_get(value, 2)?)?,
        nonnegative(heap.sequence_get(value, 3)?)?,
    )))
}

pub(in crate::aside) fn read_repeat(heap: &Heap, value: Value) -> Result<[bool; 2], AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 4 {
        return Err(AdapterError::Input(
            "image repeat requires none, x, y or both",
        ));
    }
    let mut bytes = [0; 4];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok([false, false]),
        b"x" => Ok([true, false]),
        b"y" => Ok([false, true]),
        b"both" => Ok([true, true]),
        _ => Err(AdapterError::Input(
            "image repeat requires none, x, y or both",
        )),
    }
}

pub(in crate::aside) fn read_image_repeat(
    heap: &Heap,
    value: Value,
    slice: Value,
) -> Result<[bool; 2], AdapterError> {
    let repeat = read_repeat(heap, value)?;
    if repeat != [false; 2] && slice.tag() != ValueTag::Null {
        return Err(AdapterError::Input(
            "image repeat and slice are mutually exclusive",
        ));
    }
    Ok(repeat)
}

pub(in crate::aside) fn positive(value: Value) -> Result<f64, AdapterError> {
    let value = nonnegative(value)?;
    if value == 0. {
        return Err(AdapterError::Input("image tile_scale must be positive"));
    }
    Ok(value)
}

pub(in crate::aside) fn read_corner_colors(
    heap: &Heap,
    value: Value,
) -> Result<Option<[reel_ui::peniko::Color; 4]>, AdapterError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 4
    {
        return Err(AdapterError::Input(
            "image corner_colors requires null or [top_left, top_right, bottom_right, bottom_left]",
        ));
    }
    let mut colors = [reel_ui::peniko::Color::WHITE; 4];
    for (index, output) in colors.iter_mut().enumerate() {
        let [r, g, b, a] = color(heap, heap.sequence_get(value, index as u32)?)?;
        *output = reel_ui::peniko::Color::from_rgba8(r, g, b, a);
    }
    Ok(Some(colors))
}

pub(in crate::aside) fn drawing_component(
    value: Value,
    nonnegative: bool,
) -> Result<f32, AdapterError> {
    let value = value
        .as_float()
        .or_else(|| value.as_int().map(|v| v as f64))
        .ok_or(AdapterError::Input(
            "image color adjustment requires numbers",
        ))?;
    let component = value as f32;
    if !component.is_finite() || nonnegative && value < 0. {
        return Err(AdapterError::Input(
            "image color adjustment requires finite float32 values; saturation and brightness must be nonnegative",
        ));
    }
    Ok(component)
}

pub(in crate::aside) fn drawing_rgb(
    heap: &Heap,
    value: Value,
    nonnegative: bool,
) -> Result<[f32; 3], AdapterError> {
    if matches!(value.tag(), ValueTag::Int | ValueTag::Float) {
        return Ok([drawing_component(value, nonnegative)?; 3]);
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 3
    {
        return Err(AdapterError::Input(
            "image color adjustment requires a number or [red, green, blue]",
        ));
    }
    let mut rgb = [0.; 3];
    for (index, component) in rgb.iter_mut().enumerate() {
        *component = drawing_component(heap.sequence_get(value, index as u32)?, nonnegative)?;
    }
    Ok(rgb)
}

pub(in crate::aside) fn read_image_adjustment(
    heap: &Heap,
    fields: Value,
) -> Result<reel_ui::frame::ImageColorAdjustment, AdapterError> {
    Ok(reel_ui::frame::ImageColorAdjustment {
        saturation: drawing_component(heap.table_entry(fields, 8)?.1, true)?,
        gain: drawing_rgb(heap, heap.table_entry(fields, 9)?.1, true)?,
        offset: drawing_rgb(heap, heap.table_entry(fields, 10)?.1, false)?,
    })
}

pub(in crate::aside) fn validate_image_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    match index {
        0 => {
            let info = heap.info(value)?;
            if matches!(info.kind, ObjectKind::String | ObjectKind::Bytes) && info.len != 0 {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            }
        }
        1 => read_fit(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        2 => read_slice(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        3 => validate_nonnegative(heap, value),
        4 => read_repeat(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        5 => positive(value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        6 => read_corner_colors(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        7 => boolean(value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        8 => drawing_component(value, true)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        9 | 10 => drawing_rgb(heap, value, index == 9)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        11 => read_source_rect(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        12 => read_alpha_layout(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        13 => read_sampling(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        _ => validate_layout_property(heap, index - registry::IMAGE_PREFIX.len() as u32, value),
    }
}

pub(in crate::aside) fn button_kind(
    heap: &Heap,
    label: Value,
    callback: Value,
    focus_on_pointer: Value,
    focusable: Value,
    read_limit: usize,
) -> Result<NodeKind, AdapterError> {
    validate_button_property(heap, 1, callback)?;
    Ok(NodeKind::Button {
        label: if label.tag() == ValueTag::Null {
            None
        } else {
            Some(read_text(heap, label, read_limit)?)
        },
        listening: callback.tag() != ValueTag::Null,
        focus_on_pointer: focus_on_pointer
            .as_bool()
            .ok_or(AdapterError::Input("focus_on_pointer must be boolean"))?,
        focusable: focusable
            .as_bool()
            .ok_or(AdapterError::Input("focusable must be boolean"))?,
    })
}
pub(in crate::aside) fn validate_button_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    match index {
        0 | 1 if value.tag() == ValueTag::Null => Ok(()),
        0 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::String, value)? => Ok(()),
        1 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::Closure, value)? => Ok(()),
        0 | 1 => Err(HeapError::TypeMismatch),
        2 | 3 => value.as_bool().map(|_| ()).ok_or(HeapError::TypeMismatch),
        _ => validate_layout_property(
            heap,
            index - appearance::ButtonField::PROPERTIES.len() as u32,
            value,
        ),
    }
}

pub(in crate::aside) fn validate_callback(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 1, value)
}

pub(in crate::aside) fn number(value: Value) -> Result<f64, AdapterError> {
    let value = value
        .as_float()
        .or_else(|| value.as_int().map(|n| n as f64))
        .ok_or(AdapterError::Input("node property requires a number"))?;
    if !value.is_finite() {
        return Err(AdapterError::Input("node geometry must be finite"));
    }
    Ok(value)
}
pub(in crate::aside) fn nonnegative(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if value < 0. {
        return Err(AdapterError::Input("node size must be nonnegative"));
    }
    Ok(value)
}
pub(in crate::aside) fn dimension(value: Value) -> Result<Option<f64>, AdapterError> {
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        nonnegative(value).map(Some)
    }
}
pub(in crate::aside) fn opacity(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input("opacity must be between zero and one"));
    }
    Ok(value)
}
pub(in crate::aside) fn boolean(value: Value) -> Result<bool, AdapterError> {
    value
        .as_bool()
        .ok_or(AdapterError::Input("node property requires bool"))
}
pub(in crate::aside) fn color(heap: &Heap, value: Value) -> Result<[u8; 4], AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || !matches!(info.len, 7 | 9) {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut bytes = [0; 9];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    if bytes[0] != b'#' {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut result = [255; 4];
    for (channel, pair) in result
        .iter_mut()
        .zip(bytes[1..info.len as usize].as_chunks::<2>().0.iter())
    {
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(AdapterError::Input("color requires hexadecimal channels")),
        };
        *channel = hex(pair[0])? * 16 + hex(pair[1])?;
    }
    Ok(result)
}
pub(in crate::aside) fn validate_text_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    if index == 0 {
        return super::super::rich_text::source(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch);
    }
    if index == 1 {
        return validate_button_property(heap, 1, value);
    }
    validate_layout_property(heap, index - 2, value)
}
pub(in crate::aside) fn validate_layout_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    if index as usize >= NodeProperty::PROPERTIES.len() {
        return if (index as usize)
            < NodeProperty::PROPERTIES.len() + interaction::Field::NAMES.len()
            && value.as_bool().is_some()
        {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        };
    }
    NodeProperty::validate(heap, index, value)
}
pub(in crate::aside) fn validation<T>(result: Result<T, AdapterError>) -> Result<(), HeapError> {
    result.map(|_| ()).map_err(|error| match error {
        AdapterError::Heap(error) => error,
        _ => HeapError::TypeMismatch,
    })
}
pub(in crate::aside) fn validate_number(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(number(value))
}
pub(in crate::aside) fn validate_dimension(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(dimension(value))
}
pub(in crate::aside) fn invert(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input("invert must be between zero and one"));
    }
    Ok(value)
}
pub(in crate::aside) fn validate_invert(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(invert(value))
}
pub(in crate::aside) fn blend(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::frame::BlendMode, HeapError> {
    use reel_ui::frame::BlendMode;
    use reel_ui::peniko::{Compose, Mix};
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 11 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 11];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    Ok(match &bytes[..info.len as usize] {
        b"normal" => BlendMode::default(),
        b"add" => reel_ui::peniko::BlendMode::new(Mix::Normal, Compose::PlusLighter).into(),
        b"subtract" => BlendMode::Subtract,
        b"multiply" => Mix::Multiply.into(),
        b"screen" => Mix::Screen.into(),
        b"overlay" => Mix::Overlay.into(),
        b"hard_light" => Mix::HardLight.into(),
        b"color_dodge" => Mix::ColorDodge.into(),
        b"lighten" => Mix::Lighten.into(),
        b"difference" => Mix::Difference.into(),
        b"soft_light" => Mix::SoftLight.into(),
        b"linear_burn" => BlendMode::LinearBurn,
        _ => return Err(HeapError::TypeMismatch),
    })
}
pub(in crate::aside) fn validate_blend(heap: &Heap, value: Value) -> Result<(), HeapError> {
    blend(heap, value).map(|_| ())
}
pub(in crate::aside) fn validate_opacity(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(opacity(value))
}
pub(in crate::aside) fn mask_progress(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input(
            "mask_progress must be between zero and one",
        ));
    }
    Ok(value)
}
pub(in crate::aside) fn mask_offset(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(value as f32).is_finite() {
        return Err(AdapterError::Input(
            "mask offset must be representable as finite f32",
        ));
    }
    Ok(value)
}
pub(in crate::aside) fn validate_mask_offset(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(mask_offset(value))
}
pub(in crate::aside) fn validate_mask_progress(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(mask_progress(value))
}
pub(in crate::aside) fn validate_mask(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        return Ok(());
    }
    if super::super::effects::is_effect(heap, value) {
        return super::super::effects::validate_mask(heap, value);
    }
    let info = heap.info(value)?;
    if info.kind == ObjectKind::String && info.len != 0 {
        Ok(())
    } else {
        super::super::masks::validate(heap, value)
    }
}
pub(in crate::aside) fn mask_changed(
    heap: &Heap,
    stamp: Option<(HeapHandle, u64)>,
) -> Result<bool, HeapError> {
    let Some((handle, _)) = stamp else {
        return Ok(false);
    };
    if super::super::effects::is_effect(heap, heap.import_handle(handle)?) {
        super::super::effects::changed(heap, stamp)
    } else {
        Ok(false)
    }
}
pub(in crate::aside) fn mask_handle(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
) -> Result<Option<(HeapHandle, u64)>, HeapError> {
    let value = overrides::get(heap, fields, offset, selected, NodeProperty::Mask.index())?;
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        if super::super::effects::is_effect(heap, value) {
            super::super::effects::stamp(heap, value)
        } else {
            heap.handle(value).map(|handle| Some((handle, 0)))
        }
    }
}
pub(in crate::aside) fn effect_stamp(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
) -> Result<Option<(HeapHandle, u64)>, HeapError> {
    super::super::effects::stamp(
        heap,
        overrides::get(heap, fields, offset, selected, NodeProperty::Effect.index())?,
    )
}
pub(in crate::aside) fn font_weight(value: Value) -> Result<f64, AdapterError> {
    let weight = number(value)?;
    if (1. ..=1000.).contains(&weight) {
        Ok(weight)
    } else {
        Err(AdapterError::Input(
            "font_weight must be between 1 and 1000",
        ))
    }
}
pub(in crate::aside) fn validate_font_weight(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(font_weight(value))
}
pub(in crate::aside) fn validate_effect_resolution_scale(
    _: &Heap,
    value: Value,
) -> Result<(), HeapError> {
    validation(effect_resolution_scale(value).map(|_| ()))
}
fn effect_resolution_scale(value: Value) -> Result<f64, AdapterError> {
    let scale = number(value)?;
    if scale > 0. && scale <= 1. {
        Ok(scale)
    } else {
        Err(AdapterError::Input(
            "effect_resolution_scale must be in (0, 1]",
        ))
    }
}
pub(in crate::aside) fn text_spacing(value: Value) -> Result<f64, AdapterError> {
    let spacing = number(value)?;
    if (spacing as f32).is_finite() {
        Ok(spacing)
    } else {
        Err(AdapterError::Input(
            "text spacing must be representable as finite f32",
        ))
    }
}
pub(in crate::aside) fn validate_text_spacing(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(text_spacing(value))
}
pub(in crate::aside) fn validate_nonnegative(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(nonnegative(value))
}
pub(in crate::aside) fn validate_color(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validation(color(heap, value))
}
pub(in crate::aside) fn validate_background(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_color(heap, value)
    }
}
pub(in crate::aside) fn validate_text_props(
    kind: &NodeKind,
    props: &NodeProps,
) -> Result<(), AdapterError> {
    if matches!(kind, NodeKind::TextInput { .. }) && props.text_gradient.is_some() {
        return Err(AdapterError::Input(
            "text_gradient applies to text and control captions",
        ));
    }
    if matches!(kind, NodeKind::TextInput { .. }) && props.writing_mode.is_vertical() {
        return Err(AdapterError::Input(
            "writing_mode for editable text must be horizontal-tb",
        ));
    }
    let source = match kind {
        NodeKind::Text(text) => Some(text.as_ref()),
        NodeKind::Button { label, .. }
        | NodeKind::Toggle { label, .. }
        | NodeKind::Radio { label, .. } => label.as_deref(),
        _ => None,
    };
    if source.is_some_and(|text| {
        props.reveal_fade_from < text.len() && !text.is_char_boundary(props.reveal_fade_from)
    }) {
        return Err(AdapterError::Input(
            "reveal_fade_from must be a UTF-8 byte boundary",
        ));
    }
    Ok(())
}

pub(in crate::aside) fn validate_optional_number(
    heap: &Heap,
    value: Value,
) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_number(heap, value)
    }
}
pub(in crate::aside) fn validate_byte_index(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.as_int().is_some_and(|n| usize::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
pub(in crate::aside) fn validate_reveal(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null || value.as_int().is_some_and(|n| usize::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

pub(in crate::aside) fn writing_mode(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::WritingMode, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 13 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 13];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"horizontal-tb" => Ok(reel_ui::WritingMode::HorizontalTb),
        b"vertical-rl" => Ok(reel_ui::WritingMode::VerticalRl),
        b"vertical-lr" => Ok(reel_ui::WritingMode::VerticalLr),
        _ => Err(HeapError::TypeMismatch),
    }
}
pub(in crate::aside) fn validate_writing_mode(heap: &Heap, value: Value) -> Result<(), HeapError> {
    writing_mode(heap, value).map(|_| ())
}

pub(in crate::aside) fn text_align(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::TextAlign, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 7];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    Ok(match &bytes[..info.len as usize] {
        b"start" => reel_ui::TextAlign::Start,
        b"end" => reel_ui::TextAlign::End,
        b"left" => reel_ui::TextAlign::Left,
        b"right" => reel_ui::TextAlign::Right,
        b"center" => reel_ui::TextAlign::Center,
        b"justify" => reel_ui::TextAlign::Justify,
        _ => return Err(HeapError::TypeMismatch),
    })
}
pub(in crate::aside) fn validate_text_align(heap: &Heap, value: Value) -> Result<(), HeapError> {
    text_align(heap, value).map(|_| ())
}
pub(in crate::aside) fn shrink_text(heap: &Heap, value: Value) -> Result<bool, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 6 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 6];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok(false),
        b"shrink" => Ok(true),
        _ => Err(HeapError::TypeMismatch),
    }
}
pub(in crate::aside) fn validate_text_fit(heap: &Heap, value: Value) -> Result<(), HeapError> {
    shrink_text(heap, value).map(|_| ())
}
pub(in crate::aside) fn stretch(
    heap: &Heap,
    value: Value,
) -> Result<reel_ui::stage::Stretch, HeapError> {
    use reel_ui::stage::Stretch;
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 4 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 4];
    text::read_fixed_bytes(heap, value, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok(Stretch::None),
        b"x" => Ok(Stretch::X),
        b"y" => Ok(Stretch::Y),
        b"both" => Ok(Stretch::Both),
        _ => Err(HeapError::TypeMismatch),
    }
}
pub(in crate::aside) fn validate_stretch(heap: &Heap, value: Value) -> Result<(), HeapError> {
    stretch(heap, value).map(|_| ())
}
pub(super) struct PreviousProperties<'a> {
    pub mask_source: Option<(HeapHandle, u64)>,
    pub effect_source: Option<(HeapHandle, u64)>,
    pub props: &'a NodeProps,
}

pub(super) struct PropertyContext<'a> {
    pub previous: Option<PreviousProperties<'a>>,
    pub appearance_origin: Option<Arc<str>>,
    pub effects: &'a crate::aside::source::EffectProducts,
    pub effect_projection: &'a mut crate::aside::effects::ProjectionCache,
}

pub(super) fn read_props(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
    read_limit: usize,
    context: PropertyContext<'_>,
) -> Result<NodeProps, AdapterError> {
    let PropertyContext {
        previous,
        appearance_origin,
        effects,
        effect_projection,
    } = context;
    let appearance = interaction_appearance::Selection::new(heap, fields, offset, selected)?;
    let table = heap.table_read(fields)?;
    let get = |property: NodeProperty| -> Result<Value, AdapterError> {
        let index = property.index();
        if let Some(value) = overrides::selected_value(heap, selected, index)? {
            return Ok(value);
        }
        Ok(match appearance.get(index) {
            Some(value) => value,
            None => table.value((offset + index) as u32)?,
        })
    };
    let background = get(NodeProperty::Background)?;
    validate_callback(heap, get(NodeProperty::OnInteraction)?)?;
    let mask = get(NodeProperty::Mask)?;
    validate_mask(heap, mask)?;
    let mask_effect = if super::super::effects::is_effect(heap, mask) {
        let instance = effect_projection.project(heap, mask, effects)?;
        if instance.as_ref().is_some_and(|effect| {
            effect.program().nodes()[effect.program().output().0 as usize].ty
                != reel_ui::stage::effect::EffectType::Float
        }) {
            return Err(AdapterError::Input("mask requires a scalar effect"));
        }
        instance
    } else {
        None
    };
    let mask = if mask.tag() == ValueTag::Null || mask_effect.is_some() {
        None
    } else if let Some(PreviousProperties {
        mask_source: Some(handle),
        props,
        ..
    }) = &previous
        && handle.0 == heap.handle(mask)?
    {
        props.mask.clone()
    } else {
        Some(super::super::masks::project(heap, mask, read_limit)?)
    };
    let effect = get(NodeProperty::Effect)?;
    let effect_stamp = super::super::effects::stamp(heap, effect)?;
    let effect = if let Some(PreviousProperties {
        effect_source,
        props,
        ..
    }) = &previous
        && *effect_source == effect_stamp
    {
        props.effect.clone()
    } else {
        effect_projection.project(heap, effect, effects)?
    };
    let default_font;
    let font = if let Some(PreviousProperties { props, .. }) = &previous {
        &props.font
    } else {
        default_font = NodeProps::default().font;
        &default_font
    };
    let mut props = NodeProps {
        semantics: reel_ui::stage::NodeSemantics::Generic,
        effect,
        effect_resolution_scale: effect_resolution_scale(get(
            NodeProperty::EffectResolutionScale,
        )?)?,
        mask_effect,
        flex: nonnegative(get(NodeProperty::Flex)?)?,
        x: number(get(NodeProperty::X)?)?,
        y: number(get(NodeProperty::Y)?)?,
        width: dimension(get(NodeProperty::Width)?)?,
        height: dimension(get(NodeProperty::Height)?)?,
        stretch: stretch(heap, get(NodeProperty::Stretch)?)?,
        max_width: dimension(get(NodeProperty::MaxWidth)?)?,
        max_height: dimension(get(NodeProperty::MaxHeight)?)?,
        align_x: opacity(get(NodeProperty::AlignX)?)?,
        align_y: opacity(get(NodeProperty::AlignY)?)?,
        opacity: opacity(get(NodeProperty::Opacity)?)?,
        invert: invert(get(NodeProperty::Invert)?)?,
        blend: blend(heap, get(NodeProperty::Blend)?)?,
        visible: boolean(get(NodeProperty::Visible)?)?,
        capture_included: boolean(get(NodeProperty::CaptureIncluded)?)?,
        enabled: boolean(get(NodeProperty::Enabled)?)?,
        modal: boolean(get(NodeProperty::Modal)?)?,
        clip: boolean(get(NodeProperty::Clip)?)?,
        decorated: boolean(get(NodeProperty::Decorated)?)?,
        reveal: get(NodeProperty::Reveal)?.as_int().map(|v| v as usize),
        reveal_fade: if get(NodeProperty::RevealFade)?.tag() == ValueTag::Null {
            None
        } else {
            Some(number(get(NodeProperty::RevealFade)?)?)
        },
        reveal_fade_from: get(NodeProperty::RevealFadeFrom)?.as_int().unwrap() as usize,
        reveal_fade_step: nonnegative(get(NodeProperty::RevealFadeStep)?)?,
        gap: nonnegative(get(NodeProperty::Gap)?)?,
        padding: nonnegative(get(NodeProperty::Padding)?)?,
        font_size: nonnegative(get(NodeProperty::FontSize)?)?,
        font_weight: font_weight(get(NodeProperty::FontWeight)?)?,
        letter_spacing: text_spacing(get(NodeProperty::LetterSpacing)?)?,
        word_spacing: text_spacing(get(NodeProperty::WordSpacing)?)?,
        font: text::read_shared_string(heap, get(NodeProperty::Font)?, read_limit, font)?,
        text_align: text_align(heap, get(NodeProperty::TextAlign)?)?,
        writing_mode: writing_mode(heap, get(NodeProperty::WritingMode)?)?,
        text_wrap: boolean(get(NodeProperty::TextWrap)?)?,
        line_height: nonnegative(get(NodeProperty::LineHeight)?)?,
        text_fit: if shrink_text(heap, get(NodeProperty::TextFit)?)? {
            reel_ui::core::TextFit::Shrink {
                min_font_size: nonnegative(get(NodeProperty::MinFontSize)?)? as f32,
            }
        } else {
            reel_ui::core::TextFit::None
        },
        text_outline: nonnegative(get(NodeProperty::TextOutline)?)?,
        text_outline_color: color(heap, get(NodeProperty::TextOutlineColor)?)?,
        text_shadow_x: number(get(NodeProperty::TextShadowX)?)?,
        text_shadow_y: number(get(NodeProperty::TextShadowY)?)?,
        text_shadow_color: color(heap, get(NodeProperty::TextShadowColor)?)?,
        color: color(heap, get(NodeProperty::Color)?)?,
        text_gradient: super::super::gradients::project(heap, get(NodeProperty::TextGradient)?)?,
        background: if background.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, background)?)
        },
        border_width: nonnegative(get(NodeProperty::BorderWidth)?)?,
        border_color: color(heap, get(NodeProperty::BorderColor)?)?,
        corner_radius: nonnegative(get(NodeProperty::CornerRadius)?)?,
        rotation: number(get(NodeProperty::Rotation)?)?,
        scale_x: number(get(NodeProperty::ScaleX)?)?,
        scale_y: number(get(NodeProperty::ScaleY)?)?,
        anchor_x: number(get(NodeProperty::AnchorX)?)?,
        anchor_y: number(get(NodeProperty::AnchorY)?)?,
        mask,
        appearance_origin,
        mask_progress: mask_progress(get(NodeProperty::MaskProgress)?)?,
        mask_softness: nonnegative(get(NodeProperty::MaskSoftness)?)?,
        mask_invert: boolean(get(NodeProperty::MaskInvert)?)?,
        mask_x: mask_offset(get(NodeProperty::MaskX)?)?,
        mask_y: mask_offset(get(NodeProperty::MaskY)?)?,
    };
    if props.mask_effect.is_none()
        && let Some(mask) = &props.mask
    {
        let unchanged = previous.and_then(|PreviousProperties { props: prior, .. }| {
            prior
                .mask
                .as_ref()
                .filter(|old| Arc::ptr_eq(mask, old))
                .map(|_| prior)
        });
        let prior = unchanged.and_then(|prior| prior.mask_effect.as_ref());
        // A construction that did not match a prepared template stays dynamic;
        // animation changes never repeat the negative structural lookup.
        if unchanged.is_some() && prior.is_none() {
            return Ok(props);
        }
        props.mask_effect = effect_projection.project_mask(
            heap,
            mask,
            [
                props.mask_progress as f32,
                props.mask_softness.min(f32::MAX as f64) as f32,
                props.mask_x as f32,
                props.mask_y as f32,
            ],
            effects,
            prior,
        )?;
    }
    Ok(props)
}

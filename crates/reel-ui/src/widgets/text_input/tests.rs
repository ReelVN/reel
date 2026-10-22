// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    dpi::{PhysicalPosition, PhysicalSize},
    peniko::Blob,
    properties::Dimensions,
    reel_text::{FontContext, LayoutContext, LineHeight, fontique::CollectionOptions},
};

// Share the immutable font fixture used by the stage's input geometry tests.
fn font() -> Blob<u8> {
    Blob::new(Arc::new(
        include_bytes!("../../../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec(),
    ))
}

fn contexts() -> (FontContext, LayoutContext<BrushIndex>) {
    let mut fonts = FontContext {
        collection: crate::reel_text::fontique::Collection::new(CollectionOptions {
            system_fonts: false,
            ..Default::default()
        }),
        source_cache: Default::default(),
    };
    fonts.collection.register_fonts(font(), None);
    (fonts, LayoutContext::new())
}

fn input(value: &str, multiline: bool) -> TextInput {
    TextInput::with_mode(Arc::<str>::from(value), multiline)
        .with_style(StyleProperty::FontFamily(
            crate::reel_text::FontFamily::from("Reel Weight Fixture").into_owned(),
        ))
        .with_style(StyleProperty::FontSize(20.))
}

fn layout(
    input: &mut TextInput,
    fonts: &mut FontContext,
    layouts: &mut LayoutContext<BrushIndex>,
    size: Size,
) {
    input.layout_at(Some(size.width as f32));
    input.editor.refresh_layout(fonts, layouts);
    input.update_geometry(size);
    input.keep_caret_visible();
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
}

#[test]
fn formatting_alignment_uses_width_without_wrapping_single_line() {
    let (mut fonts, mut layouts) = contexts();
    let size = Size::new(120., 60.);
    for source in ["AAA", "\u{200f}\u{202e}AAA\u{202c}"] {
        let rtl = source.starts_with('\u{200f}');
        for (alignment, factor) in [
            (TextAlign::Start, if rtl { 1. } else { 0. }),
            (TextAlign::Left, 0.),
            (TextAlign::Center, 0.5),
            (TextAlign::Right, 1.),
            (TextAlign::End, if rtl { 0. } else { 1. }),
        ] {
            let mut field = input(source, false)
                .with_text_alignment(alignment)
                .with_style(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.8)));
            layout(&mut field, &mut fonts, &mut layouts, size);
            let text = field.editor.try_layout().unwrap();
            assert_eq!(text.len(), 1);
            let metrics = *text.get(0).unwrap().metrics();
            close(
                metrics.offset.into(),
                (size.width - f64::from(metrics.advance)) * factor,
            );
            close(metrics.line_height.into(), 36.);
            close(field.translation().y, 12.);
        }
    }

    let mut field = input(&"A ".repeat(30), false).with_text_alignment(TextAlign::Center);
    layout(&mut field, &mut fonts, &mut layouts, size);
    assert_eq!(field.editor.try_layout().unwrap().len(), 1);
    assert!(field.scroll.content().width > size.width * 2.);
    assert_eq!(field.layout_width, Some(size.width as f32));
}

#[test]
fn formatting_wrap_switch_preserves_explicit_lines_and_horizontal_reach() {
    let (mut fonts, mut layouts) = contexts();
    let size = Size::new(70., 45.);
    let source = "A A A A A A A A A A\nAA";
    let mut wrapped = input(source, true);
    layout(&mut wrapped, &mut fonts, &mut layouts, size);
    assert!(wrapped.editor.try_layout().unwrap().len() > 2);
    let mut unwrapped = input(source, true).with_text_wrap(false);
    layout(&mut unwrapped, &mut fonts, &mut layouts, size);
    assert_eq!(unwrapped.editor.try_layout().unwrap().len(), 2);
    assert_eq!(unwrapped.editor.raw_text(), source);
    unwrapped
        .editor
        .driver(&mut fonts, &mut layouts)
        .move_to_line_end();
    unwrapped.keep_caret_visible();
    assert!(unwrapped.scroll.offset().x > 0.);
    assert_caret_visible(&unwrapped);
    unwrapped
        .editor
        .driver(&mut fonts, &mut layouts)
        .move_to_text_start();
    unwrapped.keep_caret_visible();
    close(unwrapped.scroll.offset().x, 0.);
    assert_caret_visible(&unwrapped);
}

fn assert_caret_visible(field: &TextInput) {
    let caret = rect(field.editor.cursor_geometry(1.).unwrap()) + field.translation();
    let size = field.scroll.viewport();
    assert!(
        caret.x0 >= -0.01 && caret.x1 <= size.width + 0.01,
        "{caret:?}, {size:?}"
    );
    assert!(
        caret.y0 >= -0.01 && caret.y1 <= size.height + 0.01,
        "{caret:?}, {size:?}"
    );
    let ime = field.ime_area();
    assert!(
        ime.x0 <= caret.x0 + 0.01 && ime.x1 >= caret.x1 - 1.01,
        "{ime:?}, {caret:?}"
    );
    assert!(size.to_rect().contains(ime.origin()));
    assert!(ime.x1 <= size.width && ime.y1 <= size.height);
}

#[test]
fn formatting_rtl_overflow_scrolls_to_both_caret_ends_and_ime_uses_full_bounds() {
    let (mut fonts, mut layouts) = contexts();
    // Explicit bidi controls exercise RTL layout using the deterministic Latin fixture.
    let source = format!("\u{200f}\u{202e}{}\u{202c}", "AA ".repeat(30));
    let mut field = input(&source, false);
    layout(&mut field, &mut fonts, &mut layouts, Size::new(90., 50.));
    assert!(
        field
            .editor
            .try_layout()
            .unwrap()
            .get(0)
            .unwrap()
            .metrics()
            .offset
            < -90.
    );
    assert!(field.text_x > 90.);
    for index in [0, source.len(), 0] {
        field
            .editor
            .driver(&mut fonts, &mut layouts)
            .select_byte_range(index, index);
        field.keep_caret_visible();
        assert_caret_visible(&field);
        let caret = field.editor.cursor_geometry(1.).unwrap();
        let ime = field.editor.ime_cursor_area();
        assert!(ime.x0 <= caret.x0 && ime.x1 >= caret.x1 - 1.);
        // The pointer route subtracts this same translation before shared hit testing.
        let viewport_point =
            Point::new(caret.x0 + 0.1, (caret.y0 + caret.y1) * 0.5) + field.translation();
        let point = viewport_point - field.translation();
        field
            .editor
            .driver(&mut fonts, &mut layouts)
            .move_to_point(point.x as f32, point.y as f32);
        let hit = field.editor.cursor_geometry(1.).unwrap();
        close(hit.x0, caret.x0);
        assert!(field.scroll.offset().x >= 0.);
    }
    field
        .editor
        .driver(&mut fonts, &mut layouts)
        .move_to_text_end();
    let end = field.editor.ime_cursor_area();
    assert!(end.x0 < 0. && end.x1 > end.x0, "{end:?}");
}

fn root(input: TextInput, size: PhysicalSize<u32>) -> (RenderRoot, WidgetId) {
    let input = input.prepare().with_props(Dimensions::MAX);
    let id = input.id();
    let mut root = RenderRoot::new(
        input,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size,
            scale_factor: 1.,
            default_font: Some(font()),
        },
    );
    root.redraw();
    (root, id)
}

#[test]
fn formatting_real_pointer_keeps_ltr_and_rtl_scrolled_caret_at_both_ends() {
    for source in [
        "AA ".repeat(30),
        format!("\u{200f}\u{202e}{}\u{202c}", "AA ".repeat(30)),
    ] {
        let (mut root, id) = root(input(&source, false), PhysicalSize::new(90, 50));
        root.focus_on(Some(id));
        for key in [NamedKey::End, NamedKey::Home, NamedKey::End] {
            assert_eq!(
                root.handle_text_event(TextEvent::key_down(Key::Named(key))),
                Handled::Yes
            );
            root.redraw();
            let field = root
                .get_widget(id)
                .unwrap()
                .downcast::<TextInput>()
                .unwrap();
            assert_caret_visible(&field);
            let caret = rect(field.editor.cursor_geometry(1.).unwrap()) + field.translation();
            let point = field
                .ctx
                .to_window(Point::new(caret.x0 + 0.1, caret.center().y));
            let pointer = PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            };
            let state = PointerState {
                position: PhysicalPosition::new(point.x, point.y),
                ..Default::default()
            };
            assert_eq!(
                root.handle_pointer_event(PointerEvent::Down(PointerButtonEvent {
                    pointer,
                    state: state.clone(),
                    button: Some(PointerButton::Primary),
                })),
                Handled::Yes
            );
            root.handle_pointer_event(PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }));
            root.redraw();
            let field = root
                .get_widget(id)
                .unwrap()
                .downcast::<TextInput>()
                .unwrap();
            let clicked = rect(field.editor.cursor_geometry(1.).unwrap()) + field.translation();
            close(clicked.x0, caret.x0);
            close(clicked.y0, caret.y0);
            assert_caret_visible(&field);
        }
    }
}

#[test]
fn formatting_dynamic_updates_keep_selection_composition_and_idle_layout() {
    for multiline in [false, true] {
        dynamic_updates(multiline);
    }
}

fn dynamic_updates(multiline: bool) {
    let source: Arc<str> = "A A A A A A A A A A A A".into();
    let (mut root, id) = root(input(&source, multiline), PhysicalSize::new(90, 80));
    root.focus_on(Some(id));
    root.edit_widget(id, |mut widget| {
        let mut field = widget.downcast::<TextInput>();
        let (fonts, layouts) = field.ctx.text_contexts();
        field
            .widget
            .editor
            .driver(fonts, layouts)
            .select_byte_range(2, 5);
    });
    root.handle_text_event(TextEvent::Ime(Ime::Preedit("AA".into(), Some((1, 1)))));
    root.redraw();
    let field = root
        .get_widget(id)
        .unwrap()
        .downcast::<TextInput>()
        .unwrap();
    let buffer = field.editor.raw_text().to_owned();
    let selection = field.editor.raw_selection().text_range();
    let compose = field.editor.raw_compose().clone();
    let committed = field.value.clone();
    assert!(compose.is_some());
    root.edit_widget(id, |mut widget| {
        let mut field = widget.downcast::<TextInput>();
        TextInput::set_text_alignment(&mut field, TextAlign::End);
        TextInput::set_text_wrap(&mut field, false);
        TextInput::insert_style(
            &mut field,
            StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.7)),
        );
    });
    root.redraw();
    let field = root
        .get_widget(id)
        .unwrap()
        .downcast::<TextInput>()
        .unwrap();
    assert_eq!(field.editor.raw_text(), buffer);
    assert_eq!(field.editor.raw_selection().text_range(), selection);
    assert_eq!(*field.editor.raw_compose(), compose);
    assert!(Arc::ptr_eq(&field.value, &committed));
    assert_eq!(field.editor.try_layout().unwrap().len(), 1);
    close(
        field
            .editor
            .try_layout()
            .unwrap()
            .get(0)
            .unwrap()
            .metrics()
            .line_height
            .into(),
        34.,
    );
    assert_caret_visible(&field);
    let generation = field.editor.generation();
    let translation = field.translation();
    for _ in 0..8 {
        root.edit_widget(id, |mut widget| {
            let mut field = widget.downcast::<TextInput>();
            TextInput::set_text_alignment(&mut field, TextAlign::End);
            TextInput::set_text_wrap(&mut field, false);
        });
        root.redraw();
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        assert!(field.editor.generation() == generation);
        assert_eq!(field.translation(), translation);
    }
    root.handle_text_event(TextEvent::Ime(Ime::Preedit("".into(), None)));
    root.redraw();
    let field = root
        .get_widget(id)
        .unwrap()
        .downcast::<TextInput>()
        .unwrap();
    assert_eq!(field.editor.raw_text(), source.as_ref());
    assert!(field.editor.raw_compose().is_none());
    assert_eq!(field.editor.raw_selection().text_range(), 2..5);
    root.edit_widget(id, |mut widget| {
        TextInput::set_text_wrap(&mut widget.downcast::<TextInput>(), true);
    });
    root.redraw();
    let field = root
        .get_widget(id)
        .unwrap()
        .downcast::<TextInput>()
        .unwrap();
    assert_eq!(field.editor.raw_text(), source.as_ref());
    assert_eq!(field.editor.raw_selection().text_range(), 2..5);
    let lines = field.editor.try_layout().unwrap().len();
    assert!(if multiline { lines > 1 } else { lines == 1 });
}

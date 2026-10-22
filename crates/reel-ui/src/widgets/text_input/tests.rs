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
    let source = "A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A\nAA";
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
    let source: Arc<str> = "A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A".into();
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

fn command(key: &str) -> TextEvent {
    let TextEvent::Keyboard(mut event) = TextEvent::key_down(Key::Character(key.into())) else {
        unreachable!()
    };
    event.modifiers = if cfg!(target_os = "macos") {
        keyboard::Modifiers::META
    } else {
        keyboard::Modifiers::CONTROL
    };
    TextEvent::Keyboard(event)
}

#[test]
fn read_only_cancels_composition_preserves_selection_and_retains_keyboard_ownership() {
    use crate::app::RenderRootSignal;
    use std::{cell::RefCell, rc::Rc};
    for multiline in [false, true] {
        let (mut root, id) = root(input("AAA", multiline), PhysicalSize::new(180, 60));
        root.focus_on(Some(id));
        let signals = Rc::new(RefCell::new(Vec::new()));
        let output = signals.clone();
        root.global_state.signal_sink = Box::new(move |signal| output.borrow_mut().push(signal));
        root.handle_text_event(command("a"));
        root.handle_text_event(TextEvent::preedit("A".into()));
        root.redraw();
        root.edit_widget(id, |mut widget| {
            TextInput::set_read_only(&mut widget.downcast::<TextInput>(), true);
        });
        root.redraw();
        assert_eq!(root.focused_widget(), Some(id));
        assert!(root.global_state.is_text_input_focused);
        assert!(!root.global_state.is_ime_active);
        assert_eq!(
            signals
                .borrow()
                .iter()
                .filter(|s| matches!(s, RenderRootSignal::EndIme))
                .count(),
            1
        );
        for event in [
            TextEvent::key_down(Key::Character("B".into())),
            TextEvent::key_down(Key::Named(NamedKey::Backspace)),
            TextEvent::key_down(Key::Named(NamedKey::Delete)),
            command("x"),
            command("v"),
            TextEvent::ClipboardPaste("paste".into()),
            TextEvent::Ime(Ime::Preedit("preedit".into(), None)),
            TextEvent::Ime(Ime::Commit("commit".into())),
        ] {
            assert_eq!(root.handle_text_event(event), Handled::Yes);
        }
        root.handle_text_event(command("c"));
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        assert_eq!(field.editor.raw_text(), "AAA");
        assert_eq!(field.editor.raw_selection().text_range(), 0..3);
        assert!(field.editor.raw_compose().is_none());
        assert!(
            signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::ClipboardStore(s) if s == "AAA"))
        );
        assert!(
            !signals
                .borrow()
                .iter()
                .any(|s| matches!(s, RenderRootSignal::Action(..)))
        );
        root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
        let mut submit = TextEvent::key_down(Key::Named(NamedKey::Enter));
        if multiline {
            // Plain Enter remains a rejected edit, while modified Enter retains submission.
            root.handle_text_event(submit.clone());
            assert!(
                !signals
                    .borrow()
                    .iter()
                    .any(|s| matches!(s, RenderRootSignal::Action(..)))
            );
            let TextEvent::Keyboard(event) = &mut submit else {
                unreachable!()
            };
            event.modifiers = if cfg!(target_os = "macos") {
                keyboard::Modifiers::META
            } else {
                keyboard::Modifiers::CONTROL
            };
        }
        root.handle_text_event(submit);
        assert!(signals.borrow().iter().any(|s| matches!(s, RenderRootSignal::Action(a, _) if matches!(a.downcast_ref::<TextInputEvent>(), Some(TextInputEvent::Submitted(v)) if v.as_ref() == "AAA"))));
        root.edit_widget(id, |mut widget| {
            let mut field = widget.downcast::<TextInput>();
            TextInput::set_value(&mut field, "author".into());
            TextInput::set_read_only(&mut field, false);
        });
        assert!(root.global_state.is_ime_active);
        root.handle_text_event(TextEvent::Ime(Ime::Commit("B".into())));
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        assert_ne!(field.value.as_ref(), "author");
    }
}

#[test]
fn read_only_initial_focus_never_starts_ime_and_accessibility_cannot_mutate() {
    let (mut root, id) = root(
        input("AAA", false).with_read_only(true),
        PhysicalSize::new(180, 60),
    );
    root.focus_on(Some(id));
    assert!(!root.global_state.is_ime_active);
    assert!(root.global_state.is_text_input_focused);
    root.handle_access_event(crate::accesskit::ActionRequest {
        action: Action::SetValue,
        target_tree: crate::accesskit::TreeId::ROOT,
        target_node: crate::accesskit::NodeId(id.to_raw()),
        data: Some(ActionData::Value("changed".into())),
    });
    assert_eq!(
        root.get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap()
            .value
            .as_ref(),
        "AAA"
    );
    root.focus_on(None);
    assert!(!root.global_state.is_ime_active);
    assert!(!root.global_state.is_text_input_focused);
}

#[test]
fn placeholder_is_lazy_styled_cached_and_hidden_during_composition() {
    for multiline in [false, true] {
        let mut field = input("AAA", multiline)
            .with_placeholder("A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A")
            .with_text_alignment(TextAlign::End);
        let (mut fonts, mut layouts) = contexts();
        field.layout_at(Some(60.));
        field.refresh_placeholder(&mut fonts, &mut layouts);
        assert!(
            field.placeholder_layout.is_none(),
            "nonempty editor needs no hint layout"
        );
        let (mut root, id) = root(field, PhysicalSize::new(60, 60));
        root.edit_widget(id, |mut widget| {
            TextInput::set_value(&mut widget.downcast::<TextInput>(), "".into())
        });
        root.redraw();
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        let hint = field.placeholder_layout.as_ref().unwrap();
        let cached = &hint.layout as *const _;
        assert!(
            if multiline {
                hint.layout.len() > 1
            } else {
                hint.layout.len() == 1
            },
            "multiline={multiline} lines={} width={:?}",
            hint.layout.len(),
            field.layout_width
        );
        assert_eq!(field.value.as_ref(), "");
        let generation = field.editor.generation();
        for _ in 0..8 {
            root.edit_widget(id, |mut widget| {
                let mut field = widget.downcast::<TextInput>();
                TextInput::set_placeholder(
                    &mut field,
                    Some("A A A A A A A A A A A A A A A A A A A A A A A A A A A A A A".into()),
                );
                TextInput::set_selection_color(&mut field, Color::BLACK);
                TextInput::set_caret_color(&mut field, Some(Color::WHITE));
                TextInput::set_outline(
                    &mut field,
                    Some(TextOutline {
                        width: 1.,
                        color: Color::BLACK,
                    }),
                );
                TextInput::set_shadow(
                    &mut field,
                    Some(TextShadow {
                        offset: Vec2::new(2., 3.),
                        color: Color::BLACK,
                    }),
                );
            });
            root.redraw();
        }
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        assert_eq!(
            &field.placeholder_layout.as_ref().unwrap().layout as *const _,
            cached
        );
        assert!(!field.placeholder_layout.as_ref().unwrap().dirty);
        assert!(field.editor.generation() == generation);
        root.focus_on(Some(id));
        root.handle_text_event(TextEvent::preedit("AAA".into()));
        let (plan, _) = root.redraw();
        let glyphs: usize = plan
            .layers
            .iter()
            .filter_map(|layer| {
                if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
                    Some(scene)
                } else {
                    None
                }
            })
            .flat_map(|scene| {
                scene.commands().iter().filter_map(|command| {
                    if let crate::imaging::record::Command::Draw(draw) = command
                        && let crate::imaging::record::Draw::GlyphRun(run) = scene.draw_op(*draw)
                    {
                        Some(run.glyphs.len())
                    } else {
                        None
                    }
                })
            })
            .sum();
        // Three preedit glyphs, each with foreground/shadow fill and outline.
        assert_eq!(glyphs, 12);
        let field = root
            .get_widget(id)
            .unwrap()
            .downcast::<TextInput>()
            .unwrap();
        assert!(field.value.is_empty());
        assert!(field.composition.is_some());
        root.handle_text_event(TextEvent::Ime(Ime::Disabled));
        root.redraw();
        assert!(
            root.get_widget(id)
                .unwrap()
                .downcast::<TextInput>()
                .unwrap()
                .composition
                .is_none()
        );
        root.edit_widget(id, |mut widget| {
            TextInput::set_placeholder(&mut widget.downcast::<TextInput>(), None);
        });
        root.redraw();
        assert!(
            root.get_widget(id)
                .unwrap()
                .downcast::<TextInput>()
                .unwrap()
                .placeholder_layout
                .is_none()
        );
    }
}

#[test]
fn editor_marks_and_shadow_paint_reuse_the_same_glyph_positions() {
    use crate::{
        app::VisualLayerKind,
        imaging::record::{Command, Draw},
        peniko::Brush,
    };
    let selection = Color::from_rgba8(11, 22, 33, 44);
    let caret = Color::from_rgb8(55, 66, 77);
    let shadow = Color::from_rgb8(88, 99, 111);
    let (mut root, id) = root(
        input("AAA", false)
            .with_selection_color(selection)
            .with_caret_color(Some(caret))
            .with_outline(Some(TextOutline {
                width: 1.,
                color: Color::BLACK,
            }))
            .with_shadow(Some(TextShadow {
                offset: Vec2::new(2., 3.),
                color: shadow,
            })),
        PhysicalSize::new(180, 60),
    );
    root.focus_on(Some(id));
    root.handle_text_event(command("a"));
    let (plan, _) = root.redraw();
    let mut selection_found = false;
    let mut foreground = Vec::new();
    let mut shadows = Vec::new();
    for layer in &plan.layers {
        if let VisualLayerKind::Scene(scene) = &layer.kind {
            for command in scene.commands() {
                if let Command::Draw(index) = command {
                    match scene.draw_op(*index) {
                        Draw::Fill { brush, .. } if *brush == Brush::Solid(selection) => {
                            selection_found = true
                        }
                        Draw::GlyphRun(run) => {
                            let data = (run.glyphs.to_vec(), run.style.clone());
                            if run.brush == Brush::Solid(shadow) {
                                shadows.push((data, run.transform));
                            } else {
                                foreground.push((data, run.transform));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    assert!(selection_found);
    assert_eq!(shadows.len(), 2, "fill and outline share one shadow layout");
    assert_eq!(foreground.len(), shadows.len());
    for ((a, a_transform), (b, b_transform)) in shadows.iter().zip(&foreground) {
        assert_eq!(a, b);
        assert_eq!(*a_transform, Affine::translate((2., 3.)) * *b_transform);
    }
    root.handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
    let (plan, _) = root.redraw();
    assert!(plan.layers.iter().any(|layer| {
        if let VisualLayerKind::Scene(scene) = &layer.kind {
            scene.commands().iter().any(|command| matches!(command, Command::Draw(index)
                if matches!(scene.draw_op(*index), Draw::Fill { brush, .. } if *brush == Brush::Solid(caret))))
        } else { false }
    }));
}

#[test]
fn placeholder_newlines_follow_the_editor_mode() {
    let (mut fonts, mut layouts) = contexts();
    for multiline in [false, true] {
        let mut field = input("", multiline).with_placeholder("AA\r\nAA");
        field.layout_at(Some(180.));
        field.refresh_placeholder(&mut fonts, &mut layouts);
        assert_eq!(
            field.placeholder_layout.as_ref().unwrap().layout.len(),
            if multiline { 2 } else { 1 }
        );
        assert!(field.value.is_empty());
    }
}

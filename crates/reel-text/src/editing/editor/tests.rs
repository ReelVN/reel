// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{FontFamily, TextWrapMode, fontique::CollectionOptions};
use alloc::{format, sync::Arc};

fn contexts() -> (FontContext, LayoutContext<()>) {
    let mut fonts = FontContext {
        collection: crate::fontique::Collection::new(CollectionOptions {
            system_fonts: false,
            ..Default::default()
        }),
        source_cache: Default::default(),
    };
    fonts.collection.register_fonts(
        linebender_resource_handle::Blob::new(Arc::new(
            include_bytes!("../../../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec(),
        )),
        None,
    );
    (fonts, LayoutContext::new())
}

#[test]
fn password_graphemes_keep_source_byte_selection_for_navigation_hits_and_deletion() {
    let (mut fonts, mut layouts) = contexts();
    let source = "Ae\u{301}👩‍🚀中";
    let boundaries = [0, 1, 4, 15, 18];
    let mut editor = PlainEditor::<()>::new(20.);
    editor.edit_styles().insert(StyleProperty::FontFamily(
        FontFamily::from("Reel Weight Fixture").into_owned(),
    ));
    editor.set_text(source);
    assert!(editor.masked.is_none());
    editor.set_password(true);
    editor.refresh_layout(&mut fonts, &mut layouts);
    assert_eq!(editor.masked.as_ref().unwrap().text, "••••");
    assert_eq!(editor.raw_text(), source);
    for (ordinal, boundary) in boundaries.into_iter().enumerate() {
        editor
            .driver(&mut fonts, &mut layouts)
            .move_to_byte(boundary);
        assert_eq!(editor.raw_selection().focus().index(), boundary);
        let caret = editor.cursor_geometry(1.).unwrap();
        editor.driver(&mut fonts, &mut layouts).move_to_point(
            (caret.x0 + 0.1) as f32,
            ((caret.y0 + caret.y1) * 0.5) as f32,
        );
        assert_eq!(editor.raw_selection().focus().index(), boundary);
        assert_eq!(editor.layout_selection().focus().index(), ordinal * 3);
    }
    editor.driver(&mut fonts, &mut layouts).move_to_text_start();
    assert_eq!(editor.raw_selection().text_range(), 0..0);
    for boundary in &boundaries[1..] {
        editor.driver(&mut fonts, &mut layouts).select_right();
        assert_eq!(editor.raw_selection().text_range(), 0..*boundary);
    }
    assert_eq!(editor.selected_text(), Some(source));
    editor.driver(&mut fonts, &mut layouts).move_to_byte(15);
    editor.driver(&mut fonts, &mut layouts).backdelete();
    assert_eq!(editor.raw_text(), "Ae\u{301}中");
    assert_eq!(editor.raw_selection().text_range(), 4..4);
    editor.driver(&mut fonts, &mut layouts).delete();
    assert_eq!(editor.raw_text(), "Ae\u{301}");
    editor.driver(&mut fonts, &mut layouts).backdelete();
    assert_eq!(editor.raw_text(), "A");
}

#[test]
fn password_ime_and_mode_changes_keep_source_text_selection_and_cached_mask() {
    let (mut fonts, mut layouts) = contexts();
    let mut editor = PlainEditor::<()>::new(20.);
    editor.edit_styles().insert(StyleProperty::FontFamily(
        FontFamily::from("Reel Weight Fixture").into_owned(),
    ));
    editor.set_password(true);
    editor.set_text("A中");
    editor.driver(&mut fonts, &mut layouts).move_to_byte(1);
    editor
        .driver(&mut fonts, &mut layouts)
        .set_compose("e\u{301}👩‍🚀", Some((3, 3)));
    assert_eq!(editor.masked.as_ref().unwrap().text, "••••");
    assert_eq!(editor.raw_text(), "Ae\u{301}👩‍🚀中");
    assert_eq!(editor.text(), "A中");
    assert_eq!(editor.raw_selection().text_range(), 4..4);
    assert_eq!(*editor.raw_compose(), Some(1..15));
    let area = editor.ime_cursor_area();
    let caret = editor.cursor_geometry(1.).unwrap();
    assert!(area.x0 <= caret.x0 && area.x1 >= caret.x1 - 1.);
    let mask = editor.masked.as_ref().unwrap();
    let text_storage = mask.text.as_ptr();
    editor.set_width(Some(100.));
    assert!(!editor.masked.as_ref().unwrap().dirty);
    editor.refresh_layout(&mut fonts, &mut layouts);
    assert_eq!(editor.masked.as_ref().unwrap().text.as_ptr(), text_storage);
    for password in [false, true] {
        editor.set_password(password);
        editor.refresh_layout(&mut fonts, &mut layouts);
        assert_eq!(editor.raw_text(), "Ae\u{301}👩‍🚀中");
        assert_eq!(editor.raw_selection().text_range(), 4..4);
        assert_eq!(*editor.raw_compose(), Some(1..15));
    }
    editor.driver(&mut fonts, &mut layouts).finish_compose();
    assert_eq!(editor.text(), "Ae\u{301}👩‍🚀中");
    assert_eq!(editor.masked.as_ref().unwrap().text, "••••");
    editor.set_password(false);
    assert!(editor.masked.is_none());
    editor.driver(&mut fonts, &mut layouts).move_to_text_end();
    editor
        .driver(&mut fonts, &mut layouts)
        .select_to_text_start();
    assert_eq!(
        editor.raw_selection().text_range(),
        0..editor.raw_text().len()
    );
}

#[test]
fn ime_cursor_area_retains_ltr_and_rtl_overflow_before_host_clipping() {
    let (mut fonts, mut layouts) = contexts();
    for text in [
        "AA ".repeat(30),
        format!("\u{200f}\u{202e}{}\u{202c}", "AA ".repeat(30)),
    ] {
        let mut editor = PlainEditor::<()>::new(20.);
        editor.edit_styles().insert(StyleProperty::FontFamily(
            FontFamily::from("Reel Weight Fixture").into_owned(),
        ));
        editor
            .edit_styles()
            .insert(StyleProperty::TextWrapMode(TextWrapMode::NoWrap));
        editor.set_width(Some(80.));
        editor.set_text(&text);
        editor.refresh_layout(&mut fonts, &mut layouts);
        editor.driver(&mut fonts, &mut layouts).move_to_text_end();
        let caret = editor.cursor_geometry(0.).unwrap();
        let area = editor.ime_cursor_area();
        assert!(
            area.x0 < caret.x0 && area.x1 > caret.x1,
            "{area:?}, {caret:?}"
        );
        assert!(area.x1 > area.x0);
        if text.starts_with('\u{200f}') {
            assert!(area.x0 < -80., "{area:?}");
        } else {
            assert!(area.x0 > 80., "{area:?}");
        }
        editor.driver(&mut fonts, &mut layouts).select_all();
        editor
            .driver(&mut fonts, &mut layouts)
            .set_compose("AA AA AA AA AA AA AA AA", Some((0, 0)));
        let range = editor.raw_compose().clone().unwrap();
        let start = editor
            .cursor_at(range.start)
            .geometry(editor.try_layout().unwrap(), 0.);
        let end = editor
            .cursor_at(range.end)
            .geometry(editor.try_layout().unwrap(), 0.);
        let area = editor.ime_cursor_area();
        assert!(area.x0 <= start.x0.min(end.x0) && area.x1 >= start.x1.max(end.x1));
    }
}

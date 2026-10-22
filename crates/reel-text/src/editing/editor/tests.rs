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

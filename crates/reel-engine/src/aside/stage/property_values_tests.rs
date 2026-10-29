use super::*;

#[test]
fn fixed_property_decoders_preserve_values_across_small_heap_reads() {
    for limit in [1, 2, 3, 7, 4096] {
        let mut heap = Heap::new(
            aside_vm::SessionEpoch(1),
            aside_vm::HeapLimits {
                max_mutation_bytes: limit,
                ..Default::default()
            },
        )
        .unwrap();
        let mut value = |text: &str| text::alloc_string(&mut heap, text, limit as usize).unwrap();
        let values = [
            "#aB01fF80",
            "#ffffff",
            "cover",
            "nearest",
            "top_bottom",
            "both",
            "color_dodge",
            "vertical-rl",
            "justify",
            "shrink",
        ]
        .map(&mut value);
        assert_eq!(color(&heap, values[0]).unwrap(), [171, 1, 255, 128]);
        assert_eq!(color(&heap, values[1]).unwrap(), [255; 4]);
        assert_eq!(
            read_fit(&heap, values[2]).unwrap(),
            reel_ui::properties::ObjectFit::Cover
        );
        assert_eq!(
            read_sampling(&heap, values[3]).unwrap(),
            reel_ui::peniko::ImageQuality::Low
        );
        assert_eq!(
            read_alpha_layout(&heap, values[4]).unwrap(),
            reel_ui::frame::ImageAlphaLayout::TopBottom
        );
        assert_eq!(read_repeat(&heap, values[5]).unwrap(), [true; 2]);
        assert_eq!(
            stretch(&heap, values[5]).unwrap(),
            reel_ui::stage::Stretch::Both
        );
        assert_eq!(
            blend(&heap, values[6]).unwrap(),
            reel_ui::peniko::Mix::ColorDodge.into()
        );
        assert_eq!(
            writing_mode(&heap, values[7]).unwrap(),
            reel_ui::WritingMode::VerticalRl
        );
        assert_eq!(
            text_align(&heap, values[8]).unwrap(),
            reel_ui::TextAlign::Justify
        );
        assert!(shrink_text(&heap, values[9]).unwrap());
        for invalid in ["#zzzzzz", "#12345", "#123456789", "#123456🙂"] {
            let value = text::alloc_string(&mut heap, invalid, limit as usize).unwrap();
            assert!(color(&heap, value).is_err());
        }
        for invalid in ["", "linear!", "nearest!", "線形", "top_bottom!"] {
            let value = text::alloc_string(&mut heap, invalid, limit as usize).unwrap();
            assert!(read_sampling(&heap, value).is_err());
            assert!(read_alpha_layout(&heap, value).is_err());
        }
        let bytes = heap.alloc_byte_buffer(&[]).unwrap();
        assert!(read_sampling(&heap, bytes).is_err());
        assert!(read_alpha_layout(&heap, bytes).is_err());
    }
}

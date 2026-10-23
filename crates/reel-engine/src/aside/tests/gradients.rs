use super::*;
use reel_ui::core::{TextGradient, TextGradientSpace};

#[test]
fn text_gradient_source_asb_binding_validation_and_snapshot() {
    let code = r##"fn opening() {
        let paint = linear_gradient("#ffffff", "#99999980", space: "glyph");
        let state = model({paint: paint});
        let node = text("ABC", text_gradient: bind(state.paint));
        stage.replace(node);
        let mut errors = 0;
        try { let _ = linear_gradient("bad", "#ffffff"); } catch error { errors += 1; }
        try { let _ = linear_gradient("#000000", "#ffffff", space: "unknown"); } catch error { errors += 1; }
        try { let _ = text_input(text_gradient: paint); } catch error { errors += 1; }
        state.paint = linear_gradient("#112233", "#aabbcc", angle: 450);
        if errors != 3 { throw "gradient validation"; }
        narrator("ready");
    }"##;
    let expected = Some(TextGradient {
        from: [0x11, 0x22, 0x33, 255],
        to: [0xaa, 0xbb, 0xcc, 255],
        angle: 90.,
        space: TextGradientSpace::Bounds,
    });
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        assert_eq!(
            vm.stage()
                .node(vm.stage().root().unwrap())
                .unwrap()
                .props
                .text_gradient,
            expected
        );
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        assert_eq!(
            vm.stage()
                .node(vm.stage().root().unwrap())
                .unwrap()
                .props
                .text_gradient,
            expected
        );
    }
}

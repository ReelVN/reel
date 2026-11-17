use super::*;

#[test]
fn frame_rate_configuration_is_atomic_inherited_and_restored_for_source_and_asb() {
    let code = r#"fn opening() {
        game.configure(frame_rate:144);
        narrator("configured");
        for invalid in [-1,0.5,1001] {
            let mut failed=false;
            try { game.configure(title:"invalid",frame_rate:invalid); } catch e { failed=true; }
            if !failed { throw "invalid rate accepted"; }
        }
        game.configure(text_speed:35);
        narrator("preserved");
        game.configure(frame_rate:59.94);
        narrator("fractional");
        game.configure(frame_rate:"uncapped");
        narrator("uncapped");
        game.configure(frame_rate:0);
        narrator("automatic");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert_eq!(vm.game_settings().frame_rate_millihertz, Some(144_000));
        (session, vm) = scopes::restored(&mut vm, &session);
        assert_eq!(vm.game_settings().frame_rate_millihertz, Some(144_000));
        let (_, mut next) = models::prepared("fn opening(){ narrator(\"fresh\"); }", encoded);
        next.inherit_game_settings(vm.game_settings().clone());
        assert_eq!(next.game_settings().frame_rate_millihertz, Some(144_000));
        for rate in [Some(144_000), Some(59_940), Some(0), None] {
            let id = session.view().interaction.unwrap().id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
            }
            models::drive(&mut vm, &mut session, false);
            assert_eq!(vm.game_settings().frame_rate_millihertz, rate);
            assert_ne!(vm.game_settings().title, "invalid");
        }
    }
}

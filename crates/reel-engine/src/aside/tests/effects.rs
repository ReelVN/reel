use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::effect::{EffectInstance, EffectValue};
use std::sync::Arc;

fn instance(vm: &SessionVm) -> Arc<EffectInstance> {
    vm.stage()
        .node(vm.stage().root().unwrap())
        .unwrap()
        .props
        .effect
        .clone()
        .unwrap()
}
fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(vm, session, false);
}

#[test]
fn compiled_effect_bindings_reuse_definitions_and_restore_parameter_ownership() {
    let code = r##"
use reel::types::Fragment;
fn tone(pixel: Fragment, amount: float = 0.5) -> (float, float, float, float) {
    let (r, g, b, a) = pixel.sample();
    (r * amount, g, b, a)
}
fn opening() {
    let state = model({amount: 0.25});
    let paint = effect(tone, params: {amount: bind(state.amount)});
    let node = stack(width: 64, height: 64, background: "#ffffff", effect: paint);
    stage.replace(node);
    narrator("initial");
    state.amount = 0.75;
    narrator("updated");
    let mut rejected = false;
    try { state.amount = "invalid"; } catch e { rejected = true; }
    if !rejected || state.amount != 0.75 { throw "effect accepted a wrong parameter type"; }
    node.effect = null;
    narrator("cleared");
}
"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let initial = instance(&vm);
        assert_eq!(initial.values(), &[EffectValue::Float(0.25_f32.to_bits())]);
        vm.sync_stage(&session).unwrap();
        assert!(Arc::ptr_eq(&initial, &instance(&vm)));
        advance(&mut vm, &mut session);
        let updated = instance(&vm);
        assert_eq!(updated.values(), &[EffectValue::Float(0.75_f32.to_bits())]);
        assert!(Arc::ptr_eq(initial.program(), updated.program()));
        let usage = vm.vm.heap().budget_usage();
        let initial_storage = initial.storage_bytes() as u64;
        let retained_bytes = usage.charged_bytes();
        drop(initial);
        vm.sync_stage(&session).unwrap();
        assert_eq!(usage.charged_bytes(), retained_bytes - initial_storage);
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            vm.vm.program().clone(),
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(20),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap()
        .with_effect_products(vm.effect_products())
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut restored) = restore.finish().unwrap();
        assert!(Arc::ptr_eq(
            updated.program(),
            instance(&restored).program()
        ));
        assert_eq!(updated.values(), instance(&restored).values());
        advance(&mut restored, &mut session);
        assert!(
            restored
                .stage()
                .node(restored.stage().root().unwrap())
                .unwrap()
                .props
                .effect
                .is_none()
        );
    }
}

#[test]
fn shared_effect_projection_outlives_its_vm_and_refunds_on_final_ui_release() {
    let code = r#"
use reel::types::Fragment;
fn coverage(pixel: Fragment, amount: float = 0.5) -> float { amount }
fn opening() {
    let paint = effect(coverage);
    stage.replace(stack(width: 64, height: 64, effect: paint) {
        stack(width: 32, height: 32, effect: paint);
    });
    narrator("ready");
}
"#;
    let (mut session, mut vm) = models::prepared(code, false);
    models::drive(&mut vm, &mut session, false);
    let first = instance(&vm);
    let root = vm.stage().node(vm.stage().root().unwrap()).unwrap();
    let child = vm
        .stage()
        .node(root.children[0])
        .unwrap()
        .props
        .effect
        .as_ref()
        .unwrap();
    assert!(
        Arc::ptr_eq(&first, child),
        "one effect host/revision shares one parameter allocation"
    );
    let storage = first.storage_bytes() + first.program().storage_bytes();
    let usage = vm.vm.heap().budget_usage();
    drop(vm);
    let retained = usage.charged_bytes();
    assert!(retained >= storage as u64);
    assert_eq!(first.values(), &[EffectValue::Float(0.5_f32.to_bits())]);
    std::thread::spawn(move || drop(first)).join().unwrap();
    assert!(retained - usage.charged_bytes() >= storage as u64);
}

#[test]
fn texture_parameters_and_scalar_masks_update_in_source_and_asb() {
    let code = r##"
use reel::types::{Fragment, Texture};
fn channel(pixel: Fragment, ink: Texture, gain: float, paper: Texture, amount: float = 0.5) -> float {
    let (r, g, b, a) = ink.sample(pixel.uv);
    if amount > 0.5 { return ink.gray(pixel.uv) * a; }
    ink.alpha(pixel.uv) * amount * gain * paper.alpha(pixel.uv)
}
fn color(pixel: Fragment) -> (float, float, float, float) { (1.0, 0.0, 0.0, 1.0) }
fn opening() {
    let state = model({ink: texture("mask-a.png"), amount: 0.25});
    let paint = effect(channel, params: {
        ink: bind(state.ink), gain: 1.0, paper: texture("paper.png"), amount: bind(state.amount),
    });
    let node = stack(width: 64, height: 64, mask: paint);
    stage.replace(node);
    narrator("first");
    state.ink = texture("mask-b.reel-texture");
    state.amount = 0.75;
    narrator("second");
    let mut rejected = false;
    try { state.ink = "wrong-type"; } catch e { rejected = true; }
    if !rejected { throw "Texture accepted a string"; }
    rejected = false;
    try { node.mask = effect(color); } catch e { rejected = true; }
    if !rejected { throw "mask accepted RGBA output"; }
    narrator("checked");
}
"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let mask = |vm: &SessionVm| {
            vm.stage()
                .node(vm.stage().root().unwrap())
                .unwrap()
                .props
                .mask_effect
                .clone()
                .unwrap()
        };
        let first = mask(&vm);
        assert_eq!(first.textures()[0].as_ref(), "mask-a.png");
        assert_eq!(first.textures()[1].as_ref(), "paper.png");
        assert_eq!(
            first.values(),
            &[
                EffectValue::Float(1.0_f32.to_bits()),
                EffectValue::Float(0.25_f32.to_bits())
            ]
        );
        advance(&mut vm, &mut session);
        let second = mask(&vm);
        assert_eq!(second.textures()[0].as_ref(), "mask-b.reel-texture");
        assert_eq!(
            second.values(),
            &[
                EffectValue::Float(1.0_f32.to_bits()),
                EffectValue::Float(0.75_f32.to_bits())
            ]
        );
        assert!(Arc::ptr_eq(first.program(), second.program()));
        assert_eq!(
            first.textures()[0].as_ref(),
            "mask-a.png",
            "retained instance stays immutable"
        );
        advance(&mut vm, &mut session);
        assert_eq!(mask(&vm).textures()[0].as_ref(), "mask-b.reel-texture");
    }
}

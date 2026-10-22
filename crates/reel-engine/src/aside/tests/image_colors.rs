use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::{frame::ImageColorAdjustment, stage::NodeKind};

fn restore(session: &Session, vm: &mut SessionVm) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(42),
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

fn adjustment(vm: &SessionVm, image: bool) -> ImageColorAdjustment {
    vm.stage()
        .nodes()
        .find_map(|node| {
            if let NodeKind::Image {
                source, adjustment, ..
            } = &node.kind
                && (source.key() == "still.png") == image
            {
                return Some(*adjustment);
            }
            None
        })
        .unwrap()
}

fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..10000 {
        let step = vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 7,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap();
        match step {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                // Configuration validates its complete property table atomically.
                let result = vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1));
                if let Err(error) = result {
                    vm.reject_native(session, ticket, error).unwrap();
                }
                collect(vm);
            }
            SessionStep::Vm(RunOutcome::Fault(id)) => {
                panic!("color fault: {:?}", vm.vm.delivery_fault(id))
            }
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("color fixture did not settle");
}

#[test]
fn image_colors_validate_atomically_bind_and_restore_for_images_and_video() {
    let code = r#"fn opening() {
        let state=model({saturation:0.5});
        let p=image("still.png"); let v=video("movie.mp4");
        if p.saturation!=1 || p.brightness!=1 || p.color_offset!=0 || v.saturation!=1 {
            throw "color defaults";
        }
        p.with(saturation:bind(state.saturation),brightness:[2,0.5,1],color_offset:(0.1,-0.2,0.3));
        v.with(saturation:0,brightness:0.25,color_offset:[0,0.2,0]);
        let mut errors=0;
        try {p.saturation=-1;} catch error {errors+=1;}
        try {p.saturation=-1e-100;} catch error {errors+=1;}
        try {p.brightness=[1,-1,1];} catch error {errors+=1;}
        try {p.brightness=[1,2];} catch error {errors+=1;}
        try {p.color_offset=1e100;} catch error {errors+=1;}
        try {v.saturation=1e100;} catch error {errors+=1;}
        try {p.with(brightness:3,saturation:-1);} catch error {errors+=1;}
        if errors!=7 || p.brightness[0]!=2 || state.saturation!=0.5 || v.saturation!=0 {
            throw "failed colors changed previous values";
        }
        stage.replace(row(){p;v;});
        narrator("before restore");
        state.saturation=0;
        p.with(brightness:1,color_offset:0);
        v.saturation=2;
        narrator("after restore");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        assert_eq!(
            adjustment(&vm, true),
            ImageColorAdjustment {
                saturation: 0.5,
                gain: [2., 0.5, 1.],
                offset: [0.1, -0.2, 0.3]
            }
        );
        assert_eq!(
            adjustment(&vm, false),
            ImageColorAdjustment {
                saturation: 0.,
                gain: [0.25; 3],
                offset: [0., 0.2, 0.]
            }
        );
        (session, vm) = restore(&session, &mut vm);
        assert_eq!(adjustment(&vm, true).gain, [2., 0.5, 1.]);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        drive(&mut vm, &mut session);
        assert_eq!(
            adjustment(&vm, true),
            ImageColorAdjustment {
                saturation: 0.,
                ..Default::default()
            }
        );
        assert_eq!(adjustment(&vm, false).saturation, 2.);
    }
}

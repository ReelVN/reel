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
fn layered_images_use_shared_image_defaults_and_keep_slots_across_restore_and_updates() {
    use reel_ui::kurbo::Rect;
    let code = r#"fn opening() {
        let state=model({changed:false});
        let portrait=layered_image(|| if state.changed {
            [{source:"still.png",x:12,width:80,height:90,saturation:0.5,
              brightness:[2,0.5,1],color_offset:0.1,source_rect:(4,5,16,32),sampling:"nearest"}]
        } else {
            [{source:"still.png",x:7,width:80,height:90}]
        },x:23);
        stage.replace(portrait);
        narrator("defaults");
        state.changed=true;
        narrator("custom fields");
        state.changed=false;
        narrator("restored defaults");
    }"#;
    let image = |vm: &SessionVm| {
        vm.stage().nodes().find(|node|matches!(&node.kind,NodeKind::Image {source,..} if source.key()=="still.png")).unwrap().clone()
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let initial = image(&vm);
        let NodeKind::Image {
            adjustment,
            source_rect,
            sampling,
            ..
        } = initial.kind
        else {
            unreachable!()
        };
        assert_eq!(adjustment, ImageColorAdjustment::default());
        assert_eq!(source_rect, None);
        assert_eq!(sampling, reel_ui::peniko::ImageQuality::Medium);
        assert_eq!(initial.props.x, 7.);
        assert_eq!(initial.props.width, Some(80.));
        (session, vm) = restore(&session, &mut vm);
        let restored_id = image(&vm).id;
        for changed in [true, false] {
            let id = session.view().interaction.unwrap().id();
            for _ in 0..2 {
                session
                    .handle_local_input(session.root_owner(), InputAction::Advance(id))
                    .unwrap();
            }
            drive(&mut vm, &mut session);
            let current = image(&vm);
            assert_eq!(current.id, restored_id);
            let NodeKind::Image {
                adjustment,
                source_rect,
                sampling,
                ..
            } = current.kind
            else {
                unreachable!()
            };
            assert_eq!(
                sampling,
                if changed {
                    reel_ui::peniko::ImageQuality::Low
                } else {
                    reel_ui::peniko::ImageQuality::Medium
                }
            );
            if changed {
                assert_eq!(current.props.x, 12.);
                assert_eq!(source_rect, Some(Rect::new(4., 5., 20., 37.)));
                assert_eq!(
                    adjustment,
                    ImageColorAdjustment {
                        saturation: 0.5,
                        gain: [2., 0.5, 1.],
                        offset: [0.1; 3]
                    }
                );
            } else {
                assert_eq!(current.props.x, 7.);
                assert_eq!(source_rect, None);
                assert_eq!(adjustment, ImageColorAdjustment::default());
            }
        }
    }
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

#[test]
fn image_source_rect_binds_validates_and_restores_from_source_and_asb() {
    use reel_ui::kurbo::Rect;
    let code = r#"fn opening() {
        let frames=[(0,0,64,64),(64,0,64,64)];
        let state=model({frame:0});
        let p=image("still.png",source_rect:derive(||frames[state.frame]),width:80,height:90);
        let v=video("movie.mp4",source_rect:(-2,1,1,2));
        let mut errors=0;
        try {p.source_rect=(0,0,0,64);} catch error {errors+=1;}
        try {p.source_rect=(0,0,-1,64);} catch error {errors+=1;}
        try {p.source_rect=(0,0,1e100,64);} catch error {errors+=1;}
        try {p.source_rect=(0,0,1e-100,64);} catch error {errors+=1;}
        try {v.source_rect=(0,0,64,-1);} catch error {errors+=1;}
        if errors!=5 || p.source_rect[2]!=64 || v.source_rect[0]!=-2 {throw "invalid source window committed";}
        stage.replace(row(){p;v;});
        narrator("before");
        state.frame=1; v.source_rect=null;
        narrator("after");
    }"#;
    fn rectangles(vm: &SessionVm) -> Vec<Option<Rect>> {
        vm.stage()
            .nodes()
            .filter_map(|n| match &n.kind {
                NodeKind::Image { source_rect, .. } => Some(*source_rect),
                _ => None,
            })
            .collect()
    }
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let expected = vec![
            Some(Rect::new(0., 0., 64., 64.)),
            Some(Rect::new(-2., 1., -1., 3.)),
        ];
        assert_eq!(rectangles(&vm), expected);
        let identities = vm.stage().nodes().map(|n| n.id).collect::<Vec<_>>();
        (session, vm) = restore(&session, &mut vm);
        assert_eq!(rectangles(&vm), expected);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        drive(&mut vm, &mut session);
        assert_eq!(
            rectangles(&vm),
            vec![Some(Rect::new(64., 0., 128., 64.)), None]
        );
        assert_eq!(
            vm.stage().nodes().map(|n| n.id).collect::<Vec<_>>(),
            identities
        );
    }
}

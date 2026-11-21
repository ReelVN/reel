use super::*;

fn setup(bound: bool) -> (Session, SessionVm) {
    let position = if bound { "bind(position.y)" } else { "0" };
    let code = format!(
        r#"
    let state=model({{phase:"idle"}});
    fn opening() {{
        let position=model({{y:0.0}});
        let values=[];for i in 0..80 {{values.append(i);}}
        let rows=list(||values,key:|x|x,virtual:true,build:|row|column(height:40){{button(f"row {{row.value}}");}});
        let viewport=scroll(width:320,height:120,scroll_y:{position}){{rows}};
        stage.replace(column{{
            viewport;text(value:derive(||state.phase));
            button("start",on_click:||{{
                state.phase="down";
                let down=await tween(viewport,scroll_y:640,duration:0.8,easing:"linear").run();
                if down!="Completed" {{state.phase=down;return;}}
                state.phase="up";
                state.phase=await tween(viewport,scroll_y:0,duration:0.8,easing:"linear").run();
            }});
            button("same",on_click:||{{viewport.scroll_y=viewport.scroll_y;}});
            button("jump",on_click:||{{let target=rows.scroll_to(4);}});
        }});
        narrator("ready");
    }}"#
    );
    let (mut session, mut vm) = models::prepared(&code, true);
    drive(&mut vm, &mut session);
    for _ in 0..3 {
        feedback(&mut vm, &mut session, 0., 320., &[]);
    }
    (session, vm)
}
fn offset(vm: &SessionVm) -> f64 {
    let (_, owner) = ids(vm);
    let NodeKind::Scroll { offset, .. } = vm.stage().node(owner).unwrap().kind else {
        panic!()
    };
    offset.y
}
fn phase(vm: &SessionVm, expected: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|node| matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == expected))
}
fn tick(vm: &mut SessionVm, session: &mut Session, time: f64) {
    super::super::activities::advance_clock(vm, session, time, 64);
    drive(vm, session);
}
fn position(vm: &SessionVm) -> aside_vm::PropertyId {
    let (_, owner) = ids(vm);
    aside_vm::PropertyId {
        host: vm.stage.value(vm.vm.heap(), owner).unwrap(),
        field: stage::scroll::ScrollField::Y.index() as u32,
    }
}
fn scroll(vm: &mut SessionVm, session: &mut Session, input: bool, actual: f64, content: f64) {
    let (_, owner) = ids(vm);
    let event = NodeScroll {
        input,
        node: owner,
        requested: Vec2::new(0., offset(vm)),
        offset: Vec2::new(0., actual),
        viewport: Size::new(320., 120.),
        content: Size::new(320., content),
    };
    vm.interactions(session, &[NodeEvent::Scroll(event)])
        .unwrap();
    drive(vm, session);
}

#[test]
fn unchanged_virtual_anchor_feedback_does_not_publish_an_author_write() {
    let (mut session, mut vm) = setup(false);
    let property = position(&vm);
    vm.vm.heap_mut().track_host_writes(property.host).unwrap();
    let version = vm
        .vm
        .heap()
        .host_write_version(property.host, property.field)
        .unwrap();
    feedback(&mut vm, &mut session, 0., 300., &[]);
    assert_eq!(
        vm.vm
            .heap()
            .host_write_version(property.host, property.field)
            .unwrap(),
        version
    );
    assert_eq!(offset(&vm), 0.);
}

#[test]
fn virtual_refills_and_measurement_changes_preserve_both_author_scroll_tween_durations() {
    for bound in [false, true] {
        let (mut session, mut vm) = setup(bound);
        click(&mut vm, &mut session, "start");
        for step in 1..=16 {
            let time = f64::from(step) / 10.;
            tick(&mut vm, &mut session, time);
            let expected = if step <= 8 {
                time * 800.
            } else {
                (1.6 - time) * 800.
            };
            assert!(
                (offset(&vm) - expected).abs() < 1e-7,
                "bound={bound} time={time}"
            );
            let current = offset(&vm);
            if step == 2 {
                let mut demand = event(&vm, current, 320., &[]);
                let mut measurements = demand.measurements.to_vec();
                measurements[0].height += 40.;
                demand.measurements = measurements.into();
                vm.interactions(&mut session, &[NodeEvent::VirtualList(demand)])
                    .unwrap();
                drive(&mut vm, &mut session);
                assert_eq!(
                    offset(&vm),
                    current,
                    "implicit anchor compensation yields to the active writer"
                );
            }
            let current = offset(&vm);
            feedback(&mut vm, &mut session, current, 320., &[]);
            assert!(
                phase(
                    &vm,
                    if step < 8 {
                        "down"
                    } else if step < 16 {
                        "up"
                    } else {
                        "Completed"
                    }
                ),
                "bound={bound} time={time}"
            );
        }
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn layout_clamp_keeps_author_activity_ownership_and_manual_scroll_takes_it_over() {
    for bound in [false, true] {
        let (mut session, mut vm) = setup(bound);
        click(&mut vm, &mut session, "start");
        tick(&mut vm, &mut session, 0.2);
        let property = position(&vm);
        let version = vm
            .vm
            .heap()
            .host_write_version(property.host, property.field)
            .unwrap();
        scroll(&mut vm, &mut session, false, 120., 240.);
        assert_eq!(offset(&vm), 120.);
        assert_eq!(
            vm.vm
                .heap()
                .host_write_version(property.host, property.field)
                .unwrap(),
            version
        );
        tick(&mut vm, &mut session, 0.3);
        assert!((offset(&vm) - 240.).abs() < 1e-7);
        assert!(phase(&vm, "down"));
        let current = offset(&vm);
        scroll(&mut vm, &mut session, true, current, 3200.);
        tick(&mut vm, &mut session, 0.31);
        assert!(
            phase(&vm, "Stopped"),
            "a user scroll intent takes ownership even at the same position"
        );
        assert!(!vm.activities_need_time());
    }
}

#[test]
fn direct_same_value_assignment_and_explicit_list_positioning_still_stop_scroll_tweens() {
    for command in ["same", "jump"] {
        let (mut session, mut vm) = setup(true);
        click(&mut vm, &mut session, "start");
        tick(&mut vm, &mut session, 0.2);
        click(&mut vm, &mut session, command);
        tick(&mut vm, &mut session, 0.21);
        assert!(phase(&vm, "Stopped"), "{command}");
        assert!(!vm.activities_need_time());
    }
}

use super::*;
use crate::aside::checkpoint::{CheckpointBinding, CheckpointLoad, CheckpointRecord};
use crate::aside::{
    NativeBudget, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits, source,
};
use crate::{Epoch, Session, SessionId};
use reel_ui::stage::NodeKind;

fn fixture(version: &str, slot: &str, update: &str, prefix: u32, reward: u32) -> String {
    format!(
        r#"
        use std::resume::{{state,root,flow,point}};
        use reel::types::Completion;
        class World {{let mut score:int=0;}}
        class Facade {{
            let page:any; let callback:any;
            fn init(self,page,callback){{self.page=page;self.callback=callback;}}
        }}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())){{
            let selection=completion();
            let scene=Facade(text("{version} scene"),||"{version} scene callback");
            let poem=Facade(button("select",on_click:||{{selection.resolve(w.score+{reward});}}),
                ||"{version} poem callback");
            let ctx={{selection:selection,page:poem.page,{slot}scene:scene}};
            stage.replace(stack(){{scene.page;poem.page;}});
            run_flow(chapter,w,context:ctx)
        }}
        fn chapter(w:World,ctx){{
            flow("poetry");w.score+={prefix};{update}
            let scene=ctx.scene;let poem=ctx["诗歌"];let page=ctx.page;
            let context_alias=ctx;let poem_alias=poem;
            let selection:Completion=ctx.selection;
            point("selection");let result=await selection;
            if ctx!=context_alias||scene!=ctx.scene||poem!=poem_alias||poem!=ctx["诗歌"]
                ||page!=poem.page||selection!=ctx.selection{{throw "current member alias lost";}}
            if scene.callback()!="{version} scene callback"||poem.callback()!="{version} poem callback"{{
                throw "wrong current callback";
            }}
            result
        }}
        "#
    )
}

fn fresh(code: &str, epoch: u64, encoded: bool) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "context_slots", epoch, "main").unwrap();
    if encoded {
        story.program = aside_vm::bytecode::VerifiedProgram::decode(
            &story
                .program
                .encode(Default::default())
                .unwrap()
                .into_bytes(),
            Default::default(),
        )
        .unwrap();
    }
    let initializer = story.initializer;
    let session = Session::new(SessionId(919), Epoch(epoch)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    (session, vm)
}

fn settle(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..50_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(64))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            SessionStep::Vm(outcome) => return outcome,
            outcome => panic!("unexpected context fixture outcome {outcome:?}"),
        }
    }
    panic!("context fixture did not settle")
}

fn save(encoded: bool) -> Vec<u8> {
    capture(
        &fixture(
            "A",
            r#""诗歌":null,seed:poem,"#,
            r#"ctx["诗歌"]=ctx.seed;ctx.remove("seed");"#,
            7,
            10,
        ),
        &["scene", "诗歌", "page", "selection"],
        encoded,
    )
}

fn capture(code: &str, members: &[&str], encoded: bool) -> Vec<u8> {
    let (mut session, mut vm) = fresh(code, 1, encoded);
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    vm.start_root(&session, None).unwrap();
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    let mut capture = vm.capture_checkpoint_begin(&session).unwrap();
    let mut ready = false;
    for _ in 0..50_000 {
        if capture
            .step(&mut vm, &session, NativeBudget::RetryMinimum(1), 1)
            .unwrap()
        {
            ready = true;
            break;
        }
    }
    assert!(ready, "context capture exceeded its work budget");
    let captured = capture.finish(&mut vm).unwrap();
    let bindings: Vec<_> = captured.record().bindings().iter().flatten().collect();
    assert!(
        bindings
            .iter()
            .any(|binding| matches!(binding, CheckpointBinding::CurrentContext(_)))
    );
    for &name in members {
        assert!(bindings.iter().any(|binding| matches!(binding, CheckpointBinding::CurrentContextMember { member, .. } if member == name)), "missing recorded member {name}");
    }
    let mut bytes = Vec::new();
    captured.record().write_to(&mut bytes).unwrap();
    bytes
}

fn restore(
    code: &str,
    bytes: &[u8],
    encoded: bool,
) -> Result<(Session, SessionVm), CheckpointError> {
    let (mut session, mut vm) = fresh(code, 2, encoded);
    let mut load = CheckpointLoad::new(CheckpointRecord::decode(bytes).unwrap(), &vm)?;
    for _ in 0..50_000 {
        if load.step(
            &mut vm,
            &mut session,
            "en",
            NativeBudget::RetryMinimum(1),
            1,
            SliceBudget {
                instructions: 1,
                ..Default::default()
            },
            || 0,
        )? {
            return Ok((session, vm));
        }
    }
    panic!("context load exceeded its work budget")
}

#[test]
fn typed_context_unpacking_keeps_automatic_wait_and_current_actor_aliases() {
    let published = r#"
        use std::resume::{root, flow};
        class World { let mut score: int = 0; }
        root(main, "game");
        fn main(world: World = model(World())) {
            let selection = completion();
            let title = text("A actor");
            let actor = button("select", on_click: || { selection.resolve(world.score + 35); });
            stage.replace(column { title; actor; });
            let services = {selection, title, actor};
            run_flow(outer, world, context: services)
        }
        fn outer(world: World, context: {selection: Completion, title: TextNode, actor: Node}) {
            flow("story");
            let reward = chapter(world, context);
            reward
        }
        fn chapter(world: World, context: {selection: Completion, title: TextNode, actor: Node}) {
            flow("chapter");
            let {selection, title: caption, actor} = context;
            world.score += 7;
            let reward = await selection;
            if selection != context.selection || caption != context.title || actor != context.actor {
                throw "current stage aliases lost";
            }
            if caption.value != "CURRENT" { throw "old caption restored"; }
            reward
        }
    "#;
    let edited = published
        .replace("A actor", "B actor")
        .replace("CURRENT", "B actor")
        .replace("world.score += 7", "world.score += 700")
        .replace(
            "let services = {selection, title, actor};",
            "let rebuilt = {actor, title, selection};",
        )
        .replace("context: services", "context: rebuilt");
    for encoded in [false, true] {
        let bytes = capture(published, &["selection", "title", "actor"], encoded);
        let (mut session, mut vm) = restore(&edited, &bytes, encoded).unwrap();
        assert!(vm.main_wait.is_some());
        let actor = vm
            .stage()
            .nodes()
            .find(|node| matches!(&node.kind, NodeKind::Button { .. }))
            .unwrap()
            .id;
        assert!(vm.click(&mut session, actor).unwrap());
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("automatic wait did not resume through the current context");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(42));
    }
}

#[test]
fn named_context_members_reconnect_current_callbacks_aliases_and_completion_source_asb() {
    for encoded in [false, true] {
        let bytes = save(encoded);
        let b = fixture("B", r#""诗歌":poem,"#, "", 700, 35);
        let (mut session, mut vm) = restore(&b, &bytes, encoded).unwrap();
        assert!(
            vm.main_wait.is_some(),
            "current completion still waits for selection"
        );
        let button = vm.stage().nodes().find(|node| {
            matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "select")
        }).unwrap().id;
        assert!(vm.click(&mut session, button).unwrap());
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("current selection did not complete its restored waiter");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(42));

        for slot in ["", r#""诗歌":null,"#] {
            let b = fixture("B", slot, "", 700, 35);
            let Err(error) = restore(&b, &bytes, encoded) else {
                panic!("load accepted an absent current member");
            };
            assert!(
                matches!(
                    error,
                    CheckpointError::Data(TypedDataError::Field { ref path, .. })
                        if path == r#"run_flow.context["诗歌"]"#
                ),
                "{error}"
            );
        }
    }
}

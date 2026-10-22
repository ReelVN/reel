use super::*;
use reel_ui::{
    input::ActionPhase,
    stage::{NodeAction, NodeKind},
};

fn prepared(game: &str, encoded: bool) -> (Session, SessionVm) {
    let main = format!(
        r#"
        use game::{game};
        fn main() {{
            let root=stack(width:960,height:640);stage.replace(root);
            let result={game}.play(root);
            root.add(text(f"{{result.game}}:{{result.outcome}}"));
            narrator("arcade returned");
        }}
    "#
    );
    prepared_main(&main, encoded)
}

fn prepared_main(main: &str, encoded: bool) -> (Session, SessionVm) {
    let inputs = [
        ("main", main),
        (
            "simulation",
            include_str!("../../../../../examples/author_arcade/src/simulation.aside"),
        ),
        (
            "pong",
            include_str!("../../../../../examples/author_arcade/src/pong.aside"),
        ),
        (
            "bricks",
            include_str!("../../../../../examples/author_arcade/src/bricks.aside"),
        ),
    ]
    .into_iter()
    .map(|(name, source)| aside_compiler::semantic::SourceModule {
        canonical_path: format!("game::{name}"),
        source_path: Some(format!("{name}.aside")),
        version: "1".into(),
        source: source.into(),
    })
    .collect::<Vec<_>>();
    let compiled = source::compile_project(&inputs, &Default::default(), 1).unwrap_or_else(
        |error| match error {
            source::SourceError::ProjectAnalysis(analysis) => panic!("{:?}", analysis.diagnostics),
            error => panic!("{error:?}"),
        },
    );
    let module = compiled
        .modules
        .iter()
        .find(|module| module.canonical_path() == "game::main")
        .unwrap();
    let function=module.functions().iter().position(|function| matches!(
        module.hir().node(function.node).form,
        aside_compiler::hir::Form::Function {name:Some(name),..} if module.hir().text(name.span)=="main"
    )).unwrap();
    let output = compiled
        .compilation
        .modules
        .iter()
        .find(|module| module.metadata.canonical_path == "game::main")
        .unwrap()
        .metadata
        .functions[function]
        .output;
    let mut story = source::CompiledStory {
        program: compiled.compilation.program,
        initializer: compiled.compilation.initializer,
        entry: output,
        catalogs: compiled.catalogs,
    };
    if encoded {
        let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
        story.program = bc::verify(
            bc::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
    }
    let mut session = session();
    let (initializer, entry) = (story.initializer, story.entry);
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    models::drive(&mut vm, &mut session, false);
    vm.start(&session, entry).unwrap();
    models::drive(&mut vm, &mut session, false);
    (session, vm)
}

#[test]
fn arcade_project_starts_with_its_story_dialogue() {
    for encoded in [false, true] {
        let (_, vm) = prepared_main(
            include_str!("../../../../../examples/author_arcade/src/main.aside"),
            encoded,
        );
        assert!(vm.stage().nodes().any(|node| matches!(
            &node.kind,
            NodeKind::Text(text) if text.starts_with("The cafe is quiet.")
        )));
    }
}

fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Button {
                label: Some(value), ..
            } if value == label => Some(node.id),
            _ => None,
        })
        .unwrap();
    vm.click(session, id).unwrap();
    models::drive(vm, session, false);
}

#[test]
fn arcade_games_move_pause_resume_and_return_to_the_story() {
    for encoded in [false, true] {
        for game in ["pong", "bricks"] {
            let (mut session, mut vm) = prepared(game, encoded);
            let movement = if game == "pong" { "move_y" } else { "move_x" };
            let (id, bindings) = vm
                .stage()
                .nodes()
                .find_map(|node| match &node.kind {
                    NodeKind::Input { bindings, .. }
                        if bindings
                            .iter()
                            .any(|binding| binding.name.as_ref() == movement) =>
                    {
                        Some((node.id, bindings.clone()))
                    }
                    _ => None,
                })
                .unwrap();
            vm.input_action(
                &mut session,
                &NodeAction {
                    node: id,
                    action: movement.into(),
                    value: 1.,
                    phase: ActionPhase::Changed,
                    bindings: bindings.clone(),
                },
            )
            .unwrap();
            let position = |vm: &SessionVm| {
                vm.stage()
                    .nodes()
                    .find_map(|node| {
                        if game == "pong" && node.props.x == 24. && node.props.width == Some(14.) {
                            Some(node.props.y)
                        } else if game == "bricks"
                            && node.props.y == 550.
                            && node.props.width == Some(120.)
                        {
                            Some(node.props.x)
                        } else {
                            None
                        }
                    })
                    .unwrap()
            };
            let initial = position(&vm);
            for frame in 1..=6 {
                activities::advance_clock(&mut vm, &session, frame as f64 / 60., 64);
                models::drive(&mut vm, &mut session, false);
            }
            assert!(
                position(&vm) > initial,
                "held action moves the paddle: {game}"
            );
            click(&mut vm, &mut session, "Pause  [P / Start]");
            let paused = position(&vm);
            activities::advance_clock(&mut vm, &session, 10., 64);
            models::drive(&mut vm, &mut session, false);
            assert_eq!(position(&vm), paused);
            click(&mut vm, &mut session, "Resume");
            activities::advance_clock(&mut vm, &session, 10.02, 64);
            models::drive(&mut vm, &mut session, false);
            assert!(position(&vm) > paused);
            vm.input_action(
                &mut session,
                &NodeAction {
                    node: id,
                    action: "serve".into(),
                    value: 1.,
                    phase: ActionPhase::Pressed,
                    bindings,
                },
            )
            .unwrap();
            models::drive(&mut vm, &mut session, false);
            let ball_x = |vm: &SessionVm| {
                vm.stage()
                    .nodes()
                    .find(|node| node.props.width == Some(12.) && node.props.height == Some(12.))
                    .unwrap()
                    .props
                    .x
            };
            for frame in 1..=60 {
                activities::advance_clock(&mut vm, &session, 10.02 + frame as f64 / 60., 64);
                models::drive(&mut vm, &mut session, false);
            }
            let approaching = ball_x(&vm);
            for frame in 61..=84 {
                activities::advance_clock(&mut vm, &session, 10.02 + frame as f64 / 60., 64);
                models::drive(&mut vm, &mut session, false);
            }
            if game == "pong" {
                assert!(
                    ball_x(&vm) < approaching,
                    "the rival paddle returns the ball"
                );
            } else {
                let score = vm
                    .stage()
                    .nodes()
                    .find_map(|node| match &node.kind {
                        NodeKind::Text(value) if value.starts_with("BRICKS") => {
                            value.split_whitespace().nth(1).unwrap().parse::<u32>().ok()
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    score > 0,
                    "a ball collision removes a brick and increments the score"
                );
            }
            click(&mut vm, &mut session, "Pause  [P / Start]");
            click(&mut vm, &mut session, "Return to story");
            let label = if game == "pong" {
                "Pong:left"
            } else {
                "Bricks:left"
            };
            assert!(
                vm.stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text(label.into()))
            );
            assert!(!vm.activities_need_time());
            assert_eq!(
                vm.stage().nodes().count(),
                2,
                "only the story root and result remain"
            );
        }
    }
}

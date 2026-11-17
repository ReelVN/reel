use super::*;

fn analyzed(code: &str) -> h::AnalyzedModule {
    let (mut resolver, hosts) = super::super::environment(std::env::consts::OS);
    aside_compiler::analyze_module(
        code,
        &aside_compiler::AnalysisOptions {
            module: "game::effects".into(),
            source_path: Some("src/effects.aside".into()),
            version: "1".into(),
            hosts,
            ..Default::default()
        },
        &mut resolver,
    )
    .unwrap()
    .into_module()
    .unwrap()
}
fn compile(code: &str) -> Result<CompiledEffect, EffectDiagnostic> {
    let module = analyzed(code);
    let entry = EffectFunction {
        module: 0,
        function: h::FunctionId((module.functions().len() - 1) as u32),
    };
    compile_effect(&[module], entry, EffectCompileLimits::default())
}

#[test]
fn typed_source_helpers_loops_and_source_sampling_form_one_program() {
    let code = r#"
use std::math::{sin as wave, atan2, hypot, min, max};
fn rotate(x: float, y: float, time: float) -> float {
    wave(atan2(x: x, y: y) + time) * hypot(y: y, x: x)
}
fn surface(pixel, time: float, strength: float = 0.2) -> (float, float, float, float) {
    let (u, v) = pixel.uv;
    let mut value = rotate(u, v, time);
    for i in 0..5 { value += wave(value + time); }
    if value < 0.0 { return (0.0, 0.0, 0.0, 1.0); }
    let (r, g, b, a) = pixel.sample((u, v));
    (min(r * (1.0 + value * strength), 1.0), max(g, 0.25), b, a)
}
"#;
    let output = compile(code).unwrap();
    assert_eq!(
        output
            .program
            .parameters()
            .iter()
            .map(|p| p.name.as_ref())
            .collect::<Vec<_>>(),
        ["time", "strength"]
    );
    assert_eq!(
        output.program.parameters()[1].default,
        Some(Constant::Float(0.2_f32.to_bits()))
    );
    assert_eq!(output.program.nodes().len(), output.node_sources.len());
    assert!(matches!(
        output.program.nodes()[output.program.output().0 as usize].op,
        Op::Select { .. }
    ));
    assert_eq!(
        output
            .program
            .nodes()
            .iter()
            .filter(|n| matches!(n.op, Op::SourceSample { .. }))
            .count(),
        1
    );
    assert_eq!(
        output
            .program
            .nodes()
            .iter()
            .filter(|n| matches!(n.op, Op::Unary { op: Unary::Sin, .. }))
            .count(),
        6
    );
    assert!(
        output
            .node_sources
            .iter()
            .any(|source| &code[aside_compiler::byte_range(source.span)] == "atan2(x: x, y: y)")
    );
    for expected in [Binary::Min, Binary::Max] {
        assert!(
            output
                .program
                .nodes()
                .iter()
                .any(|node| matches!(node.op, Op::Binary { op, .. } if op == expected))
        );
    }
}

#[test]
fn bound_names_and_branch_updates_preserve_source_meaning() {
    let output = compile(
        r#"
use std::math::sin as native_wave;
fn sin(value: float) -> float { value * 2.0 }
fn surface(pixel, time: float) -> (float,float,float,float) {
    let wave = native_wave;
    let mut x = time;
    let value = if time > 0.0 { x = sin(time); x } else { x = wave(time); x };
    let (_, _, _, alpha) = pixel.sample(uv: null);
    (value, x, 0.0, alpha)
}


"#,
    )
    .unwrap();
    assert_eq!(
        output
            .program
            .nodes()
            .iter()
            .filter(|n| matches!(n.op, Op::Unary { op: Unary::Sin, .. }))
            .count(),
        1
    );
    assert!(output.program.nodes().iter().any(|n| matches!(
        n.op,
        Op::Binary {
            op: Binary::Multiply,
            ..
        }
    )));
    assert!(
        output
            .program
            .nodes()
            .iter()
            .filter(|n| matches!(n.op, Op::Select { .. }))
            .count()
            >= 2
    );
}

#[test]
fn unsupported_effect_state_and_structure_report_original_ranges() {
    for (code, expected) in [
        (
            "use std::math::to_int; fn surface(pixel, time: float) { for i in 0..to_int(time) {} (0.0,0.0,0.0,1.0) }",
            "compile-time constants",
        ),
        (
            "fn recurse(value: float) -> float { recurse(value) } fn surface(pixel, time: float) { (recurse(time),0.0,0.0,1.0) }",
            "cannot recurse",
        ),
        (
            "fn surface(pixel) { await delay(1.0); (0.0,0.0,0.0,1.0) }",
            "cannot wait",
        ),
        (
            "fn surface(pixel) { pixel.uv + (0.1,0.2) }",
            "scalar numbers",
        ),
        (
            "let state = model({value: 1.0}); fn surface(pixel) { (state.value,0.0,0.0,1.0) }",
            "external runtime state",
        ),
        (
            "fn surface(pixel) { if pixel.uv { (0.0,0.0,0.0,1.0) } else { (0.0,0.0,0.0,1.0) } }",
            "bool condition",
        ),
        (
            "fn surface(pixel: float) { (0.0,0.0,0.0,1.0) }",
            "does not satisfy float",
        ),
    ] {
        let error = compile(code).unwrap_err();
        assert!(error.message.contains(expected), "{error}");
        assert_eq!(error.module, "src/effects.aside");
        assert!(!error.span.is_empty());
        assert!(code.get(aside_compiler::byte_range(error.span)).is_some());
    }
}

#[test]
fn imported_helpers_follow_canonical_module_and_export_identities() {
    use aside_compiler::semantic::{SourceModule, analyze_project};
    let sources = [
        SourceModule { canonical_path: "game::math".into(), source_path: Some("src/math.aside".into()), version: "1".into(), source: "pub fn double(value: float) -> float { value * 2.0 }".into() },
        SourceModule { canonical_path: "game::surface".into(), source_path: Some("src/surface.aside".into()), version: "1".into(), source: "use game::math::double as scale; fn surface(pixel, time: float) { (scale(time),0.0,0.0,1.0) }".into() },
    ];
    let (mut resolver, hosts) = super::super::environment(std::env::consts::OS);
    let modules = analyze_project(
        &sources,
        &aside_compiler::AnalysisOptions {
            hosts,
            ..Default::default()
        },
        &mut resolver,
    )
    .unwrap()
    .into_modules()
    .unwrap();
    let module = modules
        .iter()
        .position(|m| m.canonical_path() == "game::surface")
        .unwrap();
    let entry = EffectFunction {
        module,
        function: h::FunctionId(0),
    };
    let output = compile_effect(&modules, entry, EffectCompileLimits::default()).unwrap();
    assert!(
        output
            .node_sources
            .iter()
            .any(|source| output.source_files[source.module].as_ref() == "src/math.aside")
    );
    assert!(output.program.nodes().iter().any(|node| matches!(
        node.op,
        Op::Binary {
            op: Binary::Multiply,
            ..
        }
    )));
}

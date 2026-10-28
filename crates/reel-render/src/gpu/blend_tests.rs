//! Evaluate the production blend expression through WGSL constant folding, without a GPU.
use wgpu::naga::{self, Expression, Literal};

fn components(module: &naga::Module, expression: naga::Handle<Expression>, output: &mut Vec<f32>) {
    match &module.global_expressions[expression] {
        Expression::Literal(Literal::F32(value)) => output.push(*value),
        Expression::Compose {
            components: values, ..
        } => {
            for value in values {
                components(module, *value, output);
            }
        }
        Expression::Splat { size, value } => {
            let mut scalar = Vec::new();
            components(module, *value, &mut scalar);
            assert_eq!(scalar.len(), 1);
            output.extend(std::iter::repeat_n(scalar[0], *size as usize));
        }
        other => panic!("WGSL did not fold the blend constant: {other:?}"),
    }
}

#[test]
fn subtract_shader_preserves_premultiplied_backdrop_alpha() {
    use reel_ui::frame::BlendMode;
    use reel_ui::peniko::{self, Compose, Mix};
    for (blend, code) in [
        (BlendMode::default(), 0),
        (
            peniko::BlendMode::new(Mix::Normal, Compose::PlusLighter).into(),
            1,
        ),
        (Mix::Multiply.into(), 2),
        (Mix::Screen.into(), 3),
        (BlendMode::Subtract, 4),
        (Mix::Overlay.into(), 5),
        (Mix::HardLight.into(), 6),
        (Mix::ColorDodge.into(), 7),
        (Mix::Lighten.into(), 8),
        (Mix::Difference.into(), 9),
        (Mix::SoftLight.into(), 10),
        (BlendMode::LinearBurn, 11),
    ] {
        assert_eq!(super::replay::blend_code(blend), Some(code));
    }
    let source = include_str!("composite.wgsl");
    let module = naga::front::wgsl::parse_str(source).unwrap();
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
    let expression = source
        .split_once("fn subtract(")
        .unwrap()
        .1
        .split_once("return ")
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0;
    // The source values include coverage, source alpha and group opacity already.
    for (s, d, expected) in [
        (
            [0.2, 0.4, 0.1, 0.5],
            [0.6, 0.3, 0.15, 1.],
            [0.4, 0., 0.05, 1.],
        ),
        (
            [0.5, 0.125, 0., 0.5],
            [0.25, 0.1, 0., 0.25],
            [0., 0., 0., 0.25],
        ),
        (
            [0.05, 0.025, 0., 0.1],
            [0.2, 0.1, 0.05, 0.25],
            [0.15, 0.075, 0.05, 0.25],
        ),
        ([0.2, 0.1, 0., 0.5], [0.; 4], [0.; 4]),
        ([0.; 4], [0.2, 0.1, 0.05, 0.25], [0.2, 0.1, 0.05, 0.25]),
        (
            [0.12, 0.06, 0.03, 0.15],
            [0.5, 0.25, 0.125, 0.5],
            [0.38, 0.19, 0.095, 0.5],
        ),
    ] {
        let shader = format!(
            "const s = vec4<f32>({},{},{},{});\nconst d = vec4<f32>({},{},{},{});\nconst result = {expression};",
            s[0], s[1], s[2], s[3], d[0], d[1], d[2], d[3]
        );
        let module = naga::front::wgsl::parse_str(&shader).unwrap();
        let result = module
            .constants
            .iter()
            .find(|(_, c)| c.name.as_deref() == Some("result"))
            .unwrap()
            .1;
        let mut actual = Vec::new();
        components(&module, result.init, &mut actual);
        assert_eq!(actual.len(), 4);
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        assert_eq!(actual[3], d[3]);
        assert!(
            actual[..3]
                .iter()
                .all(|value| *value >= 0. && *value <= actual[3])
        );
    }
}

fn expression<'a>(source: &'a str, function: &str) -> &'a str {
    source
        .split_once(&format!("{function}("))
        .unwrap()
        .1
        .split_once("return ")
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0
}

// Naga does not constant-fold mix/step/sign yet. Lower only these built-ins;
// the production blend expressions and their boundary comparisons stay intact.
fn constant_builtins(source: &str) -> String {
    let mut source = source.to_owned();
    for (name, count) in [("mix", 3), ("step", 2), ("sign", 1)] {
        while let Some(start) = source.find(&format!("{name}(")) {
            let mut depth = 0;
            let mut at = start + name.len() + 1;
            let mut args = Vec::new();
            let mut end = at;
            for (offset, byte) in source[at..].bytes().enumerate() {
                let position = start + name.len() + 1 + offset;
                match byte {
                    b'(' => depth += 1,
                    b')' if depth > 0 => depth -= 1,
                    b',' if depth == 0 => {
                        args.push(source[at..position].to_owned());
                        at = position + 1;
                    }
                    b')' => {
                        args.push(source[at..position].to_owned());
                        end = position + 1;
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(args.len(), count);
            let a = &args[0];
            let replacement = match name {
                "mix" => format!("((1.0-({t}))*({a})+({t})*({b}))", b = args[1], t = args[2]),
                "step" => format!("select(vec3(0.0),vec3(1.0),({x})>=({a}))", x = args[1]),
                "sign" => format!(
                    "select(select(vec3(-1.0),vec3(1.0),({a})>vec3(0.0)),vec3(0.0),({a})==vec3(0.0))"
                ),
                _ => unreachable!(),
            };
            source.replace_range(start..end, &replacement);
        }
    }
    source
}

fn fold(source: &str) -> Vec<f32> {
    let module = naga::front::wgsl::parse_str(&constant_builtins(source)).unwrap();
    let result = module
        .constants
        .iter()
        .find(|(_, c)| c.name.as_deref() == Some("result"))
        .unwrap()
        .1;
    let mut actual = Vec::new();
    components(&module, result.init, &mut actual);
    actual
}

#[test]
fn separable_shader_formulas_match_both_backends_at_piecewise_and_alpha_boundaries() {
    let wgsl = include_str!("composite.wgsl");
    let cg = include_str!("../gxm/shaders/composite.cg")
        .replace("float3", "vec3")
        .replace("float4", "vec4")
        .replace("lerp", "mix");
    let names = [
        "overlay",
        "hard_light",
        "color_dodge",
        "lighten",
        "difference",
        "soft_light",
        "linear_burn",
    ];
    let curve = expression(wgsl, "soft_curve");
    assert_eq!(curve, expression(&cg, "soft_curve"));
    let values = [0., 0.125, 0.25, 0.5, 0.75, 0.99999994, 1.];
    // The first returns are zero-alpha guards; the final return is source-over.
    let alpha = wgsl
        .split_once("fn separable(")
        .unwrap()
        .1
        .split_once("@fragment")
        .unwrap()
        .0
        .rsplit_once("return ")
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0;
    let cg_alpha = cg
        .split_once("vec4 separable(")
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0
        .rsplit_once("return ")
        .unwrap()
        .1
        .split_once(';')
        .unwrap()
        .0;
    assert_eq!(alpha, cg_alpha);
    for (mode, name) in names.into_iter().enumerate() {
        let production = expression(wgsl, name);
        assert_eq!(
            production,
            expression(&cg, name),
            "{name}: backend formulas differ"
        );
        let production = production.replace("soft_curve(b)", &format!("({curve})"));
        for b in values {
            for s in values {
                let hard = |b: f32, s: f32| {
                    if s <= 0.5 {
                        2. * b * s
                    } else {
                        1. - 2. * (1. - b) * (1. - s)
                    }
                };
                let expected = match mode {
                    0 => hard(s, b),
                    1 => hard(b, s),
                    2 => {
                        if b == 0. {
                            0.
                        } else if s == 1. {
                            1.
                        } else {
                            (b / (1. - s)).min(1.)
                        }
                    }
                    3 => b.max(s),
                    4 => (b - s).abs(),
                    5 => {
                        if s <= 0.5 {
                            b - (1. - 2. * s) * b * (1. - b)
                        } else {
                            let d = if b <= 0.25 {
                                ((16. * b - 12.) * b + 4.) * b
                            } else {
                                b.sqrt()
                            };
                            b + (2. * s - 1.) * (d - b)
                        }
                    }
                    6 => (b + s - 1.).max(0.),
                    _ => unreachable!(),
                };
                let shader = format!(
                    "const b = vec3<f32>({b:?}); const s = vec3<f32>({s:?}); const result = {production};"
                );
                let actual = fold(&shader);
                assert_eq!(actual.len(), 3);
                for result in actual {
                    assert!(
                        result.is_finite() && (result - expected).abs() < 2e-6,
                        "{name}({b}, {s}): {result} != {expected}"
                    );
                }
            }
        }
        // Coverage/group opacity already scales the premultiplied source here.
        for sa in [0., 0.125, 0.5, 1.] {
            for da in [0., 0.25, 1.] {
                let blended = fold(&format!(
                    "const b = vec3<f32>(0.25); const s = vec3<f32>(0.75); const result = {production};"
                ))[0];
                let srgb = 0.75 * sa;
                let drgb = 0.25 * da;
                let actual = fold(&format!(
                    "const s = vec4<f32>({srgb:?}, {srgb:?}, {srgb:?}, {sa:?}); const d = vec4<f32>({drgb:?}, {drgb:?}, {drgb:?}, {da:?}); const blended = vec3<f32>({blended:?}); const result = {alpha};"
                ));
                let expected_alpha = sa + da - sa * da;
                let expected_color = srgb * (1. - da) + drgb * (1. - sa) + sa * da * blended;
                assert!((actual[3] - expected_alpha).abs() < 1e-6);
                for value in &actual[..3] {
                    assert!(
                        (*value - expected_color).abs() < 1e-6
                            && *value >= 0.
                            && *value <= actual[3]
                    );
                }
            }
        }
    }
}

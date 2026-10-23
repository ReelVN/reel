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

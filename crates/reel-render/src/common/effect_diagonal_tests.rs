//! Fixed-axis proof, unchanged author operations and source-coordinate oracles.
use super::*;

fn program(axis: u8) -> Arc<EffectProgram> {
    let program = sampling_program(30);
    if axis == 0 {
        return program;
    }
    let mut nodes = program.nodes().to_vec();
    for node in &mut nodes {
        match &mut node.op {
            EffectOp::Extract { value, lane }
                if program.nodes()[value.0 as usize].ty == EffectType::Vec2 =>
            {
                *lane = 1 - *lane
            }
            EffectOp::Vec2(lanes) => lanes.swap(0, 1),
            _ => {}
        }
    }
    EffectProgram::new(vec![], nodes, program.output()).unwrap()
}

#[test]
fn diagonal_reductions_preserve_every_tap_expression_and_ordered_addition() {
    for axis in 0..2 {
        let program = program(axis);
        let proof = diagonal::Proof::find(&program).unwrap();
        assert_eq!(proof.axis, axis);
        let original = generate_inner(&program, Language::Cg, 1 << 20, 5).unwrap();
        let specialized =
            generate_inner_with_diagonal(&program, Language::Cg, 1 << 20, 5, Some(proof)).unwrap();
        let restored = specialized
            .function
            .lines()
            .filter(|line| !line.contains("float reel_source_fixed ="))
            .map(|line| {
                line.replace("reel_source_sample_diagonal(", "reel_source_sample(")
                    .replace(", reel_source_fixed)", ")")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            restored,
            original.function.lines().collect::<Vec<_>>().join("\n")
        );
        let fragment = compile_diagonal_fragment(&program, 1 << 20)
            .unwrap()
            .unwrap();
        let (before, body) = fragment.source.split_once("for (").unwrap();
        assert_eq!(before.matches("float reel_source_fixed =").count(), 1);
        assert_eq!(body.matches(" = reel_source_sample_diagonal(").count(), 6);
        assert!(fragment.source.contains("float mapped = dot("));
        assert_eq!(
            reduction::Reduction::find(&program, program.output())
                .unwrap()
                .iterations(),
            61
        );
        assert!(compile_diagonal_fragment(&program, 64).is_err());
    }
}

#[test]
fn unrelated_or_unproved_source_programs_keep_the_original_fragment() {
    assert!(
        compile_diagonal_fragment(&sampling_program(1), 1 << 20)
            .unwrap()
            .is_none()
    );
    assert!(
        compile_diagonal_fragment(&sampled_program(), 1 << 20)
            .unwrap()
            .is_none()
    );
    let complex =
        sampling_offsets_with_complexity(&(-30..=30).map(|n| n as f32).collect::<Vec<_>>(), true);
    assert!(
        compile_diagonal_fragment(&complex, 1 << 20)
            .unwrap()
            .is_none()
    );
    let original = sampling_program(30);
    let mut nodes = original.nodes().to_vec();
    for node in &mut nodes {
        if let EffectOp::Vec2([_, varying]) = node.op {
            node.op = EffectOp::Vec2([varying, varying]);
        }
    }
    let both = EffectProgram::new(vec![], nodes, original.output()).unwrap();
    assert!(compile_diagonal_fragment(&both, 1 << 20).unwrap().is_none());
}

// CPU reference of the same dot3 tree, not a GPU-time or texture-filter model.
// Keeping both dot evaluation orders catches reassociation-sensitive zero cases;
// equal final coordinate bits imply the same native texture filtering inputs.
fn dot(row: [f32; 3], p: [f32; 3], right_associated: bool) -> f32 {
    if right_associated {
        row[0] * p[0] + (row[1] * p[1] + row[2] * p[2])
    } else {
        (row[0] * p[0] + row[1] * p[1]) + row[2] * p[2]
    }
}

fn coordinates(
    uv: [f32; 2],
    rows: [[f32; 3]; 2],
    window: [f32; 4],
    storage: [u32; 2],
    specialized: bool,
    order: bool,
) -> [f32; 2] {
    let p = [uv[0].clamp(0., 1.), uv[1].clamp(0., 1.), 1.];
    std::array::from_fn(|axis| {
        let mut at = p;
        if specialized {
            at[1 - axis] = 0.;
        }
        let half = 0.5 / storage[axis] as f32;
        dot(rows[axis], at, order)
            .clamp(window[axis] + half, window[axis] + window[axis + 2] - half)
    })
}

fn filtered_rgba(q: [f32; 2], storage: [u32; 2]) -> [f32; 4] {
    let at = std::array::from_fn::<_, 2, _>(|i| q[i] * storage[i] as f32 - 0.5);
    let base = at.map(|n| n.floor() as u32);
    let fraction = at.map(|n| n.fract());
    let texel = |x: u32, y: u32, lane: u32| {
        ((x.wrapping_mul(37) ^ y.wrapping_mul(131) ^ lane.wrapping_mul(73)) & 255) as f32 / 255.
    };
    std::array::from_fn(|lane| {
        let upper = texel(base[0], base[1], lane as u32) * (1. - fraction[0])
            + texel(base[0] + 1, base[1], lane as u32) * fraction[0];
        let lower = texel(base[0], base[1] + 1, lane as u32) * (1. - fraction[0])
            + texel(base[0] + 1, base[1] + 1, lane as u32) * fraction[0];
        upper * (1. - fraction[1]) + lower * fraction[1]
    })
}

#[test]
fn diagonal_dot_coordinates_and_unorm_pixels_match_with_crop_padding_and_signed_zero() {
    for (storage, window, scale, offset) in [
        (
            [640, 2816],
            [0., 0., 601. / 640., 2764. / 2816.],
            [601. / 640., 2764. / 2816.],
            [0., 0.],
        ),
        (
            [128, 96],
            [16. / 128., 8. / 96., 64. / 128., 48. / 96.],
            [64. / 128., 48. / 96.],
            [16. / 128., 8. / 96.],
        ),
        (
            [128, 96],
            [16. / 128., 8. / 96., 64. / 128., 48. / 96.],
            [-64. / 128., -48. / 96.],
            [80. / 128., 56. / 96.],
        ),
        ([32, 16], [0., 0., 1., 1.], [-0., 0.], [-0., 0.]),
    ] {
        for zero in [0., -0.] {
            let rows = [[scale[0], zero, offset[0]], [zero, scale[1], offset[1]]];
            for order in [false, true] {
                for fixed in [-0., 0., f32::MIN_POSITIVE, 0.25, 0.5, 1.] {
                    for fixed_axis in 0..2 {
                        let mut original = [0.; 4];
                        let mut diagonal = [0.; 4];
                        let mut shared = None;
                        for tap in -30..=30 {
                            let mut uv = [fixed; 2];
                            uv[1 - fixed_axis] = 0.5 + tap as f32 / 17.;
                            let a = coordinates(uv, rows, window, storage, false, order);
                            let mut b = coordinates(uv, rows, window, storage, true, order);
                            if let Some(value) = shared {
                                b[fixed_axis] = value;
                            } else {
                                shared = Some(b[fixed_axis]);
                            }
                            assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
                            let sample_a = filtered_rgba(a, storage);
                            let sample_b = filtered_rgba(b, storage);
                            for lane in 0..4 {
                                original[lane] += sample_a[lane];
                                diagonal[lane] += sample_b[lane];
                            }
                        }
                        let unorm =
                            |value: f32| ((value / 61.).clamp(0., 1.) * 255. + 0.5).floor() as u8;
                        assert_eq!(original.map(unorm), diagonal.map(unorm));
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires Sony SDK offline compiler; no device or readback"]
fn diagonal_cg_compiles_both_fixed_axes_with_the_real_entry_point() {
    let compiler = gxp_compiler::Compiler::new().unwrap();
    for axis in 0..2 {
        let fragment = compile_diagonal_fragment(&program(axis), 1 << 20)
            .unwrap()
            .unwrap();
        let binary = compiler
            .compile(gxp_compiler::Stage::Fragment, &fragment.source)
            .unwrap();
        assert_eq!(&binary[..4], b"GXP\0");
        if let Some(directory) = std::env::var_os("REEL_EFFECT_SHADER_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let name = if axis == 0 { "vertical" } else { "horizontal" };
            std::fs::write(
                directory.join(format!("{name}-diagonal.cg")),
                &fragment.source,
            )
            .unwrap();
            std::fs::write(directory.join(format!("{name}-diagonal.gxp")), &binary).unwrap();
        }
    }
}

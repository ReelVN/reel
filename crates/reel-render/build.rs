fn main() {
    #[cfg(feature = "gles2")]
    compile_shaders();
}

#[cfg(feature = "gles2")]
fn compile_shaders() {
    use pvr_compiler::{Compiler, Stage};
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mut compiler = (std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita"))
        .then(|| Compiler::new().expect("Vita shader compilation requires the Linux/WSL host"));
    let variants = [
        ("paint_rgba", 0, 0),
        ("paint_coverage", 1, 0),
        ("paint_gradient", 0, 2),
        ("paint_glyph_gradient", 1, 2),
        ("paint_appearance", 0, 1),
        ("paint_packed", 3, 1),
        ("paint_video", 2, 0),
        ("paint_video_appearance", 2, 1),
    ];
    let mut shaders = Vec::new();
    for (name, stage) in [
        ("vertex", Stage::Vertex),
        ("composite_vertex", Stage::Vertex),
        ("effect_vertex", Stage::Vertex),
        ("composite", Stage::Fragment),
        ("mask", Stage::Fragment),
        ("present", Stage::Fragment),
    ] {
        let path = format!("src/gles2/shaders/{name}.glsl");
        println!("cargo:rerun-if-changed={path}");
        let source = std::fs::read_to_string(path).expect("read GLSL source");
        if name == "composite" {
            for (variant, define) in [
                ("source_over", "REEL_SOURCE_OVER_ONLY"),
                ("alpha_mask", "REEL_ALPHA_MASK_ONLY"),
                ("extended_blend", "REEL_EXTENDED_BLEND"),
            ] {
                shaders.push((
                    variant.to_owned(),
                    stage,
                    source.replacen(
                        "#version 100",
                        &format!("#version 100\n#define {define}"),
                        1,
                    ),
                ));
            }
        }
        if name == "mask" {
            for capacity in [8, 16] {
                shaders.push((
                    format!("mask_{capacity}"),
                    stage,
                    source.replacen(
                        "#version 100",
                        &format!("#version 100\n#define REEL_MASK_NODES {capacity}"),
                        1,
                    ),
                ));
            }
            for capacity in [16, 32] {
                shaders.push((
                    format!("mask_{capacity}_compact"),
                    stage,
                    source.replacen(
                        "#version 100",
                        &format!("#version 100\n#define REEL_MASK_NODES {capacity}\n#define REEL_MASK_SLOTS 8"),
                        1,
                    ),
                ));
            }
        }
        shaders.push((name.to_owned(), stage, source));
    }
    println!("cargo:rerun-if-changed=src/gles2/shaders/mask_split.glsl");
    let split = std::fs::read_to_string("src/gles2/shaders/mask_split.glsl")
        .expect("read split mask template");
    for (name, fetches, math, alpha) in [
        ("mask_split_1", 1, 8, false),
        ("mask_split_2", 2, 16, false),
        ("mask_split_2_alpha", 2, 16, true),
    ] {
        let alpha = if alpha {
            "\n#define REEL_MASK_ALPHA_ONLY"
        } else {
            ""
        };
        shaders.push((name.into(), Stage::Fragment, split.replacen("#version 100", &format!("#version 100\n#define REEL_MASK_FETCHES {fetches}\n#define REEL_MASK_MATH {math}{alpha}"), 1)));
    }
    println!("cargo:rerun-if-changed=src/gles2/shaders/mask_sample.glsl");
    println!("cargo:rerun-if-changed=src/gles2/shaders/mask_paired.glsl");
    let paired = std::fs::read_to_string("src/gles2/shaders/mask_paired.glsl")
        .expect("read paired mask template");
    for (channels, defines) in [
        ("aa", ""),
        ("ag", "\n#define REEL_MASK_SECOND_GRAY"),
        ("ga", "\n#define REEL_MASK_FIRST_GRAY"),
        (
            "gg",
            "\n#define REEL_MASK_FIRST_GRAY\n#define REEL_MASK_SECOND_GRAY",
        ),
    ] {
        for composite in [false, true] {
            let suffix = if composite { "_composite" } else { "" };
            let composite = if composite {
                "\n#define REEL_MASK_COMPOSITE"
            } else {
                ""
            };
            shaders.push((
                format!("mask_paired_{channels}{suffix}"),
                Stage::Fragment,
                paired.replacen(
                    "#version 100",
                    &format!("#version 100{defines}{composite}"),
                    1,
                ),
            ));
        }
    }
    let mask_sample = std::fs::read_to_string("src/gles2/shaders/mask_sample.glsl")
        .expect("read simple mask template");
    shaders.push((
        "mask_sample_chain".into(),
        Stage::Fragment,
        mask_sample.clone(),
    ));
    shaders.push((
        "mask_threshold".into(),
        Stage::Fragment,
        mask_sample.replacen(
            "#version 100",
            "#version 100\n#define REEL_MASK_THRESHOLD",
            1,
        ),
    ));
    for (name, defines) in [
        ("mask_sample_chain_composite", "#define REEL_MASK_COMPOSITE"),
        (
            "mask_threshold_composite",
            "#define REEL_MASK_COMPOSITE\n#define REEL_MASK_THRESHOLD",
        ),
    ] {
        shaders.push((
            name.into(),
            Stage::Fragment,
            mask_sample.replacen("#version 100", &format!("#version 100\n{defines}"), 1),
        ));
    }
    println!("cargo:rerun-if-changed=src/gles2/shaders/paint.glsl");
    let paint =
        std::fs::read_to_string("src/gles2/shaders/paint.glsl").expect("read paint template");
    for (name, alpha, regional) in [
        ("paint_direct", 0, false),
        ("paint_direct_bc1", 1, false),
        ("paint_direct_bc3", 2, false),
        ("paint_regional", 0, true),
        ("paint_regional_bc1", 1, true),
        ("paint_regional_bc3", 2, true),
        ("paint_direct_coverage", 3, false),
        ("paint_solid", 4, false),
    ] {
        let regional = if regional {
            "\n#define REEL_REGIONAL"
        } else {
            ""
        };
        shaders.push((
            name.to_owned(),
            Stage::Fragment,
            paint.replacen(
                "#version 100",
                &format!("#version 100\n#define REEL_DIRECT {alpha}{regional}"),
                1,
            ),
        ));
    }
    for (name, kind, style) in variants {
        let source = paint.replacen(
            "#version 100",
            &format!("#version 100\n#define REEL_SOURCE_KIND {kind}\n#define REEL_STYLE {style}"),
            1,
        );
        shaders.push((name.to_owned(), Stage::Fragment, source));
    }
    for (name, stage, source) in shaders {
        std::fs::write(output.join(format!("{name}.glsl")), &source).unwrap();
        let Some(compiler) = &mut compiler else {
            continue;
        };
        let compiled = compiler
            .compile_binary(stage, &source)
            .expect("compile SGX shader binary");
        assert!(
            compiled.success && !compiled.binary.is_empty(),
            "{name}: {}",
            compiled.log
        );
        std::fs::write(output.join(format!("{name}.sgx")), compiled.binary).unwrap();
    }
}

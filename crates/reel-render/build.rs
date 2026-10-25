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
        shaders.push((name.to_owned(), stage, source));
    }
    println!("cargo:rerun-if-changed=src/gles2/shaders/paint.glsl");
    let paint =
        std::fs::read_to_string("src/gles2/shaders/paint.glsl").expect("read paint template");
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

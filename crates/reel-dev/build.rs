#[cfg(feature = "device-checks")]
#[path = "src/device_checks/masks.rs"]
mod masks;

#[cfg(feature = "device-checks")]
#[path = "src/device_checks/present_color/effect_fixture.rs"]
mod effect_fixture;

#[cfg(feature = "device-checks")]
#[allow(dead_code)]
#[path = "../reel-render/build_support/shaders.rs"]
mod paint_shaders;

fn main() {
    #[cfg(feature = "device-checks")]
    prepare_device_checks();
}

#[cfg(feature = "device-checks")]
fn prepare_device_checks() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [".git/HEAD", ".git/index", "Cargo.lock", "crates"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let revision = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unavailable".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=normal"])
        .map(|status| (!status.is_empty()).to_string())
        .unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=REEL_DEVICE_CHECKS_REVISION={revision}");
    println!("cargo:rustc-env=REEL_DEVICE_CHECKS_DIRTY={dirty}");
    // Retain the player's native startup configuration in the diagnostic binary.
    // reel-player supplies the archive; the linker must keep these process symbols.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        for symbol in [
            "sceUserMainThreadStackSize",
            "vita_native_process_heap_anchor",
        ] {
            println!("cargo:rustc-link-arg-bin=reel-device-checks=-Wl,--undefined={symbol}");
        }
    }
    prepare_static_mask();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        prepare_present_color();
    }
}

#[cfg(feature = "device-checks")]
fn prepare_present_color() {
    use reel_render::effect::gxp_compiler::{Compiler, Stage};
    let compiler = Compiler::new().expect("offline Sony shader compiler");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let fragment_path = "../reel-render/src/gxm/shaders/present.cg";
    let vertex_path = "src/device_checks/present_color_vertex.cg";
    let rgba_path = "src/device_checks/present_color_rgba.cg";
    let region_path = "src/device_checks/present_color/region.cg";
    for path in [fragment_path, vertex_path, rgba_path, region_path] {
        println!("cargo:rerun-if-changed={path}");
    }
    let fragment = std::fs::read_to_string(fragment_path).unwrap();
    let vertex = std::fs::read_to_string(vertex_path).unwrap();
    for (name, stage, source) in [
        ("present-color-vertex", Stage::Vertex, vertex),
        ("present-color-pbe", Stage::Fragment, fragment.clone()),
        (
            "present-color-rgba",
            Stage::Fragment,
            std::fs::read_to_string(rgba_path).unwrap(),
        ),
        (
            "present-color-region",
            Stage::Fragment,
            std::fs::read_to_string(region_path).unwrap(),
        ),
        (
            "present-color-reference",
            Stage::Fragment,
            format!("#define REEL_SHADER_SRGB\n{fragment}"),
        ),
    ] {
        std::fs::write(
            output.join(format!("{name}.gxp")),
            compiler.compile(stage, &source).unwrap(),
        )
        .unwrap();
    }
    effect_fixture::prepare(&compiler, &output);
    for shader in paint_shaders::sources_at(std::path::Path::new("../reel-render")) {
        if [
            "vertex",
            "vertex_bounded",
            "paint_direct",
            "paint_regional",
            "paint_direct_bc3",
            "paint_regional_bc3",
            "paint_direct_bounded",
            "paint_regional_bounded",
            "paint_direct_bc1_bounded",
            "paint_regional_bc1_bounded",
            "paint_direct_bc3_bounded",
            "paint_regional_bc3_bounded",
            "paint_alpha_direct",
            "paint_alpha_region",
            "paint_adjust_direct",
            "paint_adjust_region",
            "paint_monochrome_direct",
            "paint_monochrome_region",
        ]
        .contains(
            &shader
                .name
                .strip_suffix("_unclipped")
                .unwrap_or(&shader.name),
        ) {
            std::fs::write(
                output.join(format!("paint-region-{}.gxp", shader.name)),
                compiler
                    .compile(
                        if shader.vertex {
                            Stage::Vertex
                        } else {
                            Stage::Fragment
                        },
                        &shader.source,
                    )
                    .unwrap(),
            )
            .unwrap();
        }
    }
}

#[cfg(feature = "device-checks")]
fn prepare_static_mask() {
    use reel_render::effect::{Language, compile_fragment};
    use reel_ui::{
        resources::{EffectShaderDescriptor, EffectShaderFormat},
        stage::effect::lower_mask,
    };
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mask = masks::program("mask_dag").expect("the shared mask probe is valid");
    let program = lower_mask(&mask).expect("the DAG uses the shared effect language");
    let vita = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita");
    let language = if vita { Language::Cg } else { Language::Wgsl };
    let fragment = compile_fragment(&program, language, 4 << 20)
        .expect("generate the probe through the production effect compiler");
    let (format, raw) = if vita {
        use reel_render::effect::gxp_compiler::{Compiler, Stage};
        let compiler = Compiler::new().expect("offline Sony shader compiler");
        for path in compiler.tool_files().expect("compiler inputs") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        for name in ["REEL_PSP2CGC", "SCE_PSP2_SDK_DIR"] {
            println!("cargo:rerun-if-env-changed={name}");
        }
        let binary = compiler
            .compile(Stage::Fragment, &fragment.source)
            .expect("compile the static mask at packaging time");
        (EffectShaderFormat::Gxp, binary)
    } else {
        (EffectShaderFormat::Wgsl, fragment.source.into_bytes())
    };
    let descriptor = EffectShaderDescriptor::with_textures(
        masks::STATIC_DEFINITION,
        format,
        fragment
            .parameters
            .slots
            .iter()
            .map(|slot| slot.ty)
            .collect(),
        program.textures().len() as u32,
        fragment.source_sampler,
        fragment.output_type,
        &raw,
    )
    .expect("the probe has the ordinary typed shader envelope");
    let payload = descriptor.encode_payload(&raw).unwrap();
    std::fs::write(output.join("static-mask.reel-effect"), payload).unwrap();
}

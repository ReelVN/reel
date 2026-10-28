#[cfg(feature = "vita-probe")]
#[path = "src/probe/masks.rs"]
mod masks;

fn main() {
    #[cfg(feature = "vita-probe")]
    prepare_probe();
}

#[cfg(feature = "vita-probe")]
fn prepare_probe() {
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
    println!("cargo:rustc-env=REEL_PROBE_REVISION={revision}");
    println!("cargo:rustc-env=REEL_PROBE_DIRTY={dirty}");
    // Retain the player's native startup configuration in the diagnostic binary.
    // reel-player supplies the archive; the linker must keep these process symbols.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("vita") {
        for symbol in [
            "sceUserMainThreadStackSize",
            "vita_native_process_heap_anchor",
        ] {
            println!("cargo:rustc-link-arg-bin=reel-gpu-probe=-Wl,--undefined={symbol}");
        }
    }
    prepare_static_mask();
}

#[cfg(feature = "vita-probe")]
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

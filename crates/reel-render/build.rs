#[cfg(feature = "gxm")]
#[path = "src/common/effect/gxp_compiler.rs"]
#[allow(
    dead_code,
    reason = "the same SDK runner also fingerprints CLI cache products"
)]
mod gxp_compiler;
#[cfg(any(feature = "gles2", feature = "gxm"))]
#[path = "build_support/shaders.rs"]
mod shaders;

fn main() {
    #[cfg(any(feature = "gles2", feature = "gxm"))]
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    #[cfg(feature = "gles2")]
    for shader in shaders::sources("src/gles2/shaders", "glsl", false) {
        std::fs::write(output.join(format!("{}.glsl", shader.name)), shader.source).unwrap();
    }
    #[cfg(feature = "gxm")]
    {
        use gxp_compiler::{Compiler, Stage};
        println!("cargo:rerun-if-env-changed=REEL_PSP2CGC");
        println!("cargo:rerun-if-env-changed=SCE_PSP2_SDK_DIR");
        println!("cargo:rerun-if-env-changed=PATH");
        let compiler = Compiler::new().expect("locate Sony SDK offline shader compiler");
        for path in compiler.tool_files().unwrap() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        for shader in shaders::sources("src/gxm/shaders", "cg", true) {
            let stage = if shader.vertex {
                Stage::Vertex
            } else {
                Stage::Fragment
            };
            let binary = compiler
                .compile(stage, &shader.source)
                .unwrap_or_else(|error| panic!("{}: {error}", shader.name));
            std::fs::write(output.join(format!("{}.gxp", shader.name)), binary).unwrap();
        }
    }
}

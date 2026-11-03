//! Opt-in compiler diagnostics for an installed author's project.
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires an author project, artifact directory, and Sony shader compiler"]
fn project_effects_compile_to_gxp() {
    let root = PathBuf::from(std::env::var_os("REEL_GXP_PROJECT").expect("REEL_GXP_PROJECT"));
    let output = PathBuf::from(std::env::var_os("REEL_GXP_REPORT").expect("REEL_GXP_REPORT"));
    let selected = std::env::var("REEL_GXP_EFFECT").ok();
    fs::create_dir_all(&output).unwrap();
    let project =
        reel_cli::project::load(&root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = project.check_with_progress("vita", &mut |_| {}).unwrap();
    let compiler = reel_render::effect::gxp_compiler::Compiler::new().unwrap();
    let mut count = 0;
    for definition in checked.effects.definitions() {
        let id = blake3::Hash::from(definition.id).to_hex().to_string();
        let origin = checked.effects.origin(definition.id).unwrap();
        if selected.as_ref().is_some_and(|selected| {
            selected != &id && selected.as_str() != origin.function.as_ref()
        }) {
            continue;
        }
        let fragment = reel_render::effect::compile_fragment(
            &definition.program,
            reel_render::effect::Language::Cg,
            4 << 20,
        )
        .unwrap();
        fs::write(output.join(format!("{id}.cg")), &fragment.source).unwrap();
        eprintln!(
            "{} {id}: nodes={}, source_bytes={}",
            origin.function,
            definition.program.nodes().len(),
            fragment.source.len()
        );
        let binary = compiler
            .compile(
                reel_render::effect::gxp_compiler::Stage::Fragment,
                &fragment.source,
            )
            .unwrap_or_else(|error| panic!("{}: {error}", origin.function));
        fs::write(output.join(format!("{id}.gxp")), binary).unwrap();
        count += 1;
    }
    assert!(count > 0, "no matching effect definitions");
}

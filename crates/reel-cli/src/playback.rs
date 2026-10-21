//! Shared entry for author commands and the exported platform player.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn asset_overlay(
    loaded: &crate::distribution::LoadedDistribution,
    explicit: Option<PathBuf>,
) -> Option<PathBuf> {
    explicit.or_else(|| {
        if loaded.package.distribution().is_some() {
            return loaded.loose.clone();
        }
        let resources = crate::project::locate(&loaded.root)
            .ok()
            .map(|project| project.root.join("resources"))
            .filter(|resources| resources.is_dir());
        resources.or_else(|| loaded.loose.clone())
    })
}

pub fn run(path: &Path, assets: Option<PathBuf>, saves: Option<PathBuf>) -> Result<(), String> {
    let loaded = crate::distribution::load(path).map_err(|error| error.to_string())?;
    let assets = asset_overlay(&loaded, assets);
    let story = crate::story_from_package(loaded.package).map_err(|error| error.to_string())?;
    launch(
        story,
        loaded.root,
        Some((loaded.packages, assets)),
        loaded.catalogs,
        saves,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temporary() -> tempfile::TempDir {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/playback-project-tests");
        fs::create_dir_all(&root).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }

    #[test]
    fn raw_asb_uses_project_resources_and_exports_keep_their_manifest() {
        let temporary = temporary();
        let root = temporary.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::create_dir_all(root.join("build/debug")).unwrap();
        fs::write(root.join("src/main.aside"), "fn main() { 1 }").unwrap();
        let project =
            crate::project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
        let checked = project.check().unwrap();
        let artifact = crate::build(&checked, Some("main::main")).unwrap();
        let package = root.join("build/debug/game.asb");
        fs::write(&package, &artifact.bytes).unwrap();
        let loaded = crate::distribution::load(&package).unwrap();
        assert_eq!(asset_overlay(&loaded, None), Some(project.assets()));
        let explicit = root.join("overrides");
        assert_eq!(
            asset_overlay(&loaded, Some(explicit.clone())),
            Some(explicit.clone())
        );
        let options = crate::distribution::ExportOptions {
            output: root.join("build/export/debug"),
            overwrite: false,
            resources: vec![],
            text: vec![],
            modules: Default::default(),
            entry: "main::main".into(),
            pack_bytes: 4096.try_into().unwrap(),
            player: None,
        };
        crate::distribution::export(&project, &artifact, &options).unwrap();
        let loaded = crate::distribution::load(&options.output.join("game.asb")).unwrap();
        assert_eq!(asset_overlay(&loaded, None), loaded.loose);
        assert_ne!(asset_overlay(&loaded, None), Some(project.assets()));
        assert_eq!(
            asset_overlay(&loaded, Some(explicit.clone())),
            Some(explicit)
        );
    }

    #[test]
    fn standalone_raw_asb_keeps_its_directory_without_project_resources() {
        let temporary = temporary();
        let checked = crate::check("fn main() { 1 }", "single.aside").unwrap();
        let artifact = crate::build(&checked, Some("main::main")).unwrap();
        let package = temporary.path().join("single.asb");
        fs::write(&package, &artifact.bytes).unwrap();
        let loaded = crate::distribution::load(&package).unwrap();
        assert_eq!(asset_overlay(&loaded, None), loaded.loose);
    }
}

pub fn standalone(executable: &Path, saves: Option<PathBuf>) -> Result<(), String> {
    let root = executable
        .parent()
        .ok_or("player executable has no parent directory")?;
    run(
        &root.join(crate::distribution::MAIN_PACKAGE),
        None,
        Some(saves.unwrap_or_else(|| root.join("saves"))),
    )
}

pub fn launch(
    story: reel_engine::aside::source::CompiledStory,
    root: PathBuf,
    packages: Option<(Vec<PathBuf>, Option<PathBuf>)>,
    catalogs: BTreeMap<String, String>,
    saves: Option<PathBuf>,
) -> Result<(), String> {
    #[cfg(feature = "window")]
    {
        let mut options = reel_player::shell::StoryWindowOptions::default();
        options.assets.root = root;
        options.catalogs = std::sync::Arc::new(catalogs);
        if let Some((packages, overlay)) = packages {
            options.asset_packages = Some(packages);
            options.asset_overlay = overlay;
        }
        options.save_directory = saves;
        reel_player::shell::run_story(story, options).map_err(|error| error.to_string())
    }
    #[cfg(not(feature = "window"))]
    {
        let _ = (story, root, packages, catalogs, saves);
        Err("playback requires the complete Reel tool package; download the package containing the platform player".into())
    }
}

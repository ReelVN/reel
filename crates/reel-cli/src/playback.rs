//! Shared entry for author commands and the exported platform player.
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

fn project_resources(path: &Path) -> io::Result<Option<PathBuf>> {
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(crate::project::find(root)?.map(|project| project.root.join("resources")))
}

fn asset_overlay(
    loaded: &reel_player::distribution::LoadedDistribution,
    explicit: Option<PathBuf>,
) -> Option<PathBuf> {
    explicit.or_else(|| loaded.loose.clone())
}

type ResourceFiles = BTreeMap<String, PathBuf>;

/// Prepare the current project's resource view, optionally selecting another
/// source directory through the same target rules and logical namespace.
pub fn prepare_assets(
    project_root: &Path,
    explicit: Option<PathBuf>,
    resource_target: &str,
) -> io::Result<(PathBuf, Option<ResourceFiles>)> {
    let target = reel_package::resource_target::ResourceTarget::parse(resource_target)?;
    let root = explicit.unwrap_or_else(|| project_root.join("resources"));
    let prepared =
        crate::resource_conversion::prepare_directory_if_configured(project_root, &root, &target)?;
    Ok((root, prepared.map(|prepared| prepared.files)))
}

/// Prepare a compiled source session, including cached desktop effect shaders.
/// The material preview target remains independent of the executing backend.
pub fn prepare_story_assets(
    project_root: &Path,
    explicit: Option<PathBuf>,
    resource_target: &str,
    effects: &reel_engine::aside::source::EffectProducts,
) -> io::Result<(PathBuf, Option<ResourceFiles>)> {
    let (root, files) = prepare_assets(project_root, explicit, resource_target)?;
    if effects.is_empty() {
        return Ok((root, files));
    }
    let mut files = match files {
        Some(files) => files,
        None if root.is_dir() => {
            reel_package::resource_target::ResourceTarget::parse(resource_target)?.collect(&root)?
        }
        None => BTreeMap::new(),
    };
    crate::prepare_effects::merge(
        &mut files,
        crate::prepare_effects::prepare(
            project_root,
            effects,
            crate::prepare_effects::Target::Desktop,
        )?,
    )?;
    Ok((root, Some(files)))
}

pub fn run(
    path: &Path,
    assets: Option<PathBuf>,
    saves: Option<PathBuf>,
    resource_target: String,
) -> Result<(), String> {
    let (story, mut options) = load(path, assets, resource_target)?;
    options.save_directory = saves;
    launch(story, options)
}

/// Prepare an ASB using the same resource selection as the platform player.
/// Embedding hosts can supply their own presentation, audio and storage services
/// while retaining the package's catalogs, shaders and resource conversions.
pub fn load(
    path: &Path,
    assets: Option<PathBuf>,
    resource_target: String,
) -> Result<
    (
        reel_engine::aside::source::CompiledStory,
        reel_player::player::PlayerOptions,
    ),
    String,
> {
    let project_resources = project_resources(path).map_err(|error| error.to_string())?;
    let loaded = reel_player::distribution::load(path, project_resources.as_deref())
        .map_err(|error| error.to_string())?;
    let assets = asset_overlay(&loaded, assets);
    let (assets, resource_files) = if let Some(project_root) =
        project_resources.as_ref().and_then(|root| root.parent())
        && assets.is_some()
    {
        let (root, files) = prepare_assets(project_root, assets, &resource_target)
            .map_err(|error| error.to_string())?;
        (Some(root), files)
    } else {
        (assets, None)
    };
    let story = reel_player::distribution::story_from_package(loaded.package)
        .map_err(|error| error.to_string())?;
    let mut options = reel_player::player::PlayerOptions::default();
    options.assets.root = loaded.root;
    options.asset_packages = Some(loaded.packages);
    options.asset_overlay = assets;
    options.catalogs = std::sync::Arc::new(loaded.catalogs);
    options.resource_target = resource_target;
    options.resource_files = resource_files;
    Ok((story, options))
}

pub fn standalone(executable: &Path, saves: Option<PathBuf>) -> Result<(), String> {
    let root = executable
        .parent()
        .ok_or("player executable has no parent directory")?;
    run(
        &root.join(reel_package::distribution::MAIN_PACKAGE),
        None,
        Some(saves.unwrap_or_else(|| root.join("saves"))),
        std::env::consts::OS.into(),
    )
}

/// Launch a compiled story with the shared player's resource and host settings.
pub fn launch(
    story: reel_engine::aside::source::CompiledStory,
    options: reel_player::player::PlayerOptions,
) -> Result<(), String> {
    #[cfg(feature = "window")]
    {
        reel_player::platform::desktop::run_story(story, options).map_err(|error| error.to_string())
    }
    #[cfg(not(feature = "window"))]
    {
        let _ = (story, options);
        Err("playback requires the complete Reel tool package; download the package containing the platform player".into())
    }
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
        let artifact =
            crate::build_export(&checked, "main::main", crate::BuildProfile::Debug).unwrap();
        let package = root.join("build/debug/game.asb");
        fs::write(&package, artifact.encode(None).unwrap()).unwrap();
        let loaded = reel_player::distribution::load(
            &package,
            project_resources(&package).unwrap().as_deref(),
        )
        .unwrap();
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
            resource_target: std::env::consts::OS.into(),
            pack_bytes: 4096.try_into().unwrap(),
            runtime: None,
        };
        crate::distribution::export(&project, &artifact, &options).unwrap();
        let loaded = reel_player::distribution::load(
            &options.output.join("game.asb"),
            Some(&project.assets()),
        )
        .unwrap();
        assert_eq!(asset_overlay(&loaded, None), loaded.loose);
        assert_ne!(asset_overlay(&loaded, None), Some(project.assets()));
        assert_eq!(
            asset_overlay(&loaded, Some(explicit.clone())),
            Some(explicit)
        );
    }

    #[test]
    fn standalone_raw_asb_uses_its_adjacent_resources_directory() {
        let temporary = temporary();
        let checked = crate::check("fn main() { 1 }", "single.aside").unwrap();
        let artifact = crate::build(&checked, Some("main::main")).unwrap();
        let package = temporary.path().join("single.asb");
        fs::write(&package, &artifact.bytes).unwrap();
        let loaded = reel_player::distribution::load(&package, None).unwrap();
        assert_eq!(
            asset_overlay(&loaded, None),
            Some(loaded.root.join("resources"))
        );
    }
}

//! Shared entry for author commands and the exported platform player.
use std::path::{Path, PathBuf};

pub fn run(path: &Path, assets: Option<PathBuf>, saves: Option<PathBuf>) -> Result<(), String> {
    let loaded = crate::distribution::load(path).map_err(|error| error.to_string())?;
    let story = crate::story_from_package(loaded.package).map_err(|error| error.to_string())?;
    launch(
        story,
        loaded.root,
        Some((loaded.packages, assets.or(loaded.loose))),
        saves,
    )
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
    saves: Option<PathBuf>,
) -> Result<(), String> {
    #[cfg(feature = "window")]
    {
        let mut options = reel_player::shell::StoryWindowOptions::default();
        options.assets.root = root;
        if let Some((packages, overlay)) = packages {
            options.asset_packages = Some(packages);
            options.asset_overlay = overlay;
        }
        options.save_directory = saves;
        reel_player::shell::run_story(story, options).map_err(|error| error.to_string())
    }
    #[cfg(not(feature = "window"))]
    {
        let _ = (story, root, packages, saves);
        Err("build the window player with: cargo build -p reel-cli --features window".into())
    }
}

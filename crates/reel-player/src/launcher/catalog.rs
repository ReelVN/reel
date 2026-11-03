//! Catalog IO and human-readable launcher preferences, outside the input path.
use super::{Error, Game, PREFERENCES_BYTES, Preferences};
use std::{
    fs,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const CONFIG: &str = "launcher.json";

pub(super) fn scan(root: &Path, cancelled: &AtomicBool) -> Result<Vec<Game>, Error> {
    let mut games = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let package = entry.path().join("game.asb");
        if package.is_file() {
            games.push(Game {
                title: entry.file_name().to_string_lossy().into_owned(),
                bytes: directory_size(&entry.path(), cancelled)?,
                package,
            });
        }
    }
    games.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(games)
}

fn directory_size(root: &Path, cancelled: &AtomicBool) -> Result<u64, Error> {
    let mut pending = vec![root.to_path_buf()];
    let mut bytes = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            if cancelled.load(Ordering::Relaxed) {
                return Ok(bytes);
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                // Resource volumes can exceed newlib's signed 32-bit stat size.
                bytes += crate::assets::file::metadata(entry.path().as_path())?.len();
            }
        }
    }
    Ok(bytes)
}

pub(super) fn read_preferences(root: &Path) -> Result<Preferences, Error> {
    let file = match fs::File::open(root.join(CONFIG)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Preferences::default());
        }
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(PREFERENCES_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > PREFERENCES_BYTES {
        return Err("launcher.json exceeds 64 KiB".into());
    }
    Ok(serde_json::from_str(&text)?)
}

pub(super) fn save_preferences(root: &Path, preferences: &Preferences) -> Result<(), Error> {
    fs::write(root.join(CONFIG), serde_json::to_vec_pretty(preferences)?)?;
    Ok(())
}

pub(super) enum Request {
    Scan,
    Save(Preferences),
}
pub(super) enum Reply {
    Scanned(Result<Vec<Game>, String>),
    Saved(Result<(), String>),
}

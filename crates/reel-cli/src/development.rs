//! Reuse source compilation while the compiler and project contents are unchanged.
use crate::{Checked, artifact, project};
use reel_engine::aside::source::{CatalogProducts, CompiledStory};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
struct Stamp<C> {
    key: String,
    artifact: String,
    catalogs: C,
}

/// One replaceable development artifact, with source maps and inline languages.
pub struct Cache {
    root: PathBuf,
    tool: PathBuf,
    key: String,
    target: String,
}

fn hash_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = fs::File::open(path)?;
    let mut hash = blake3::Hasher::new();
    hash.update_reader(&mut file)?;
    Ok(hash.finalize())
}

fn fingerprint(root: &Path, tool: &Path, target: &str) -> io::Result<String> {
    let files = project::source_files(root)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"Reel development cache v1\0");
    hash.update(&(target.len() as u64).to_le_bytes());
    hash.update(target.as_bytes());
    hash.update(hash_file(tool)?.as_bytes());
    for file in files {
        let name = file
            .strip_prefix(root)
            .expect("collected project source")
            .as_os_str()
            .as_encoded_bytes();
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name);
        hash.update(hash_file(&file)?.as_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}

impl Cache {
    /// Canonical source project owning this development artifact and resources.
    pub fn project_root(&self) -> &Path {
        &self.root
    }

    /// Fingerprint source contents and the running tool without parsing the project.
    pub fn probe(directory: &Path, target: &str) -> io::Result<Self> {
        Self::with_tool(directory, std::env::current_exe()?, target)
    }

    fn with_tool(directory: &Path, tool: PathBuf, target: &str) -> io::Result<Self> {
        let root = project::locate(directory)?.root;
        let key = fingerprint(&root, &tool, target)?;
        Ok(Self {
            root,
            tool,
            key,
            target: target.into(),
        })
    }

    /// A stale or incomplete cache is rebuilt through the ordinary compiler.
    pub fn read(&self) -> Option<CompiledStory> {
        let directory = self.root.join("build/dev");
        let stamp: Stamp<BTreeMap<String, String>> =
            serde_json::from_slice(&fs::read(directory.join("stamp.json")).ok()?).ok()?;
        if stamp.key != self.key {
            return None;
        }
        let bytes = fs::read(directory.join("game.asb")).ok()?;
        if reel_package::digest(&bytes) != stamp.artifact {
            return None;
        }
        let mut story = reel_player::distribution::decode_story(&bytes).ok()?;
        drop(bytes);
        story.catalogs = Arc::new(CatalogProducts {
            catalogs: stamp.catalogs,
            messages: Vec::new(),
        });
        Some(story)
    }

    /// Publish a debug ASB and its catalog index after successful compilation.
    /// A source edit during compilation leaves the previous cache untouched.
    pub fn store(&self, checked: &Checked) -> io::Result<()> {
        if fingerprint(&self.root, &self.tool, &self.target)? != self.key {
            return Ok(());
        }
        let artifact = artifact::build(checked, Some(project::ENTRY))
            .map_err(|error| io::Error::other(format!("{:?}", error)))?;
        let directory = self.root.join("build/dev");
        fs::create_dir_all(&directory)?;
        crate::output::write(
            &directory.join("game.asb"),
            &self.root.join("src/main.aside"),
            &artifact.bytes,
            true,
        )?;
        let stamp = Stamp {
            key: self.key.clone(),
            artifact: artifact.blake3,
            catalogs: &checked.catalogs.catalogs,
        };
        reel_storage::publish(&directory.join("stamp.json"), true, |file| {
            serde_json::to_writer(file, &stamp).map_err(io::Error::other)
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::create_dir(dir.path().join("resources")).unwrap();
        fs::write(
            dir.path().join("src/main.aside"),
            "fn main() { tr(\"Source\", {en: \"Translated\"}) }",
        )
        .unwrap();
        let tool = dir.path().join("reel-tool");
        fs::write(&tool, "compiler one").unwrap();
        (dir, tool)
    }

    fn checked(root: &Path) -> Checked {
        project::load(root)
            .unwrap_or_else(|e| panic!("{:?}", e.failure))
            .check()
            .unwrap()
    }

    #[test]
    fn cached_story_keeps_program_source_maps_and_inline_languages() {
        let (dir, tool) = fixture();
        let cache = Cache::with_tool(dir.path(), tool, std::env::consts::OS).unwrap();
        assert!(cache.read().is_none());
        let checked = checked(dir.path());
        cache.store(&checked).unwrap();
        let cached = cache.read().unwrap();
        assert_eq!(cached.catalogs.catalogs, checked.catalogs.catalogs);
        assert!(!cached.catalogs.catalogs.is_empty());
        assert_eq!(
            aside_vm::bytecode::encode(cached.program.package(), Default::default()).unwrap(),
            aside_vm::bytecode::encode(checked.compilation.program.package(), Default::default())
                .unwrap(),
        );
        assert!(
            cached
                .program
                .source_at(cached.entry, aside_vm::bytecode::Pc(0))
                .is_some()
        );
        fs::write(
            dir.path().join("resources/current.txt"),
            "new live resource",
        )
        .unwrap();
        let next = Cache::with_tool(dir.path(), cache.tool.clone(), std::env::consts::OS).unwrap();
        assert_eq!(next.key, cache.key);
        assert!(next.read().is_some());
        let other = Cache::with_tool(dir.path(), cache.tool.clone(), "vita").unwrap();
        assert_ne!(other.key, cache.key);
        assert!(other.read().is_none());
    }

    #[test]
    fn source_and_tool_changes_invalidate_cache_while_partial_writes_rebuild() {
        let (dir, tool) = fixture();
        let cache = Cache::with_tool(dir.path(), tool.clone(), std::env::consts::OS).unwrap();
        let original = checked(dir.path());
        cache.store(&original).unwrap();
        let main = dir.path().join("src/main.aside");
        let source = fs::read(&main).unwrap();
        let modified = fs::metadata(&main).unwrap().modified().unwrap();
        fs::write(&main, "fn main() { 42 }").unwrap();
        fs::File::options()
            .write(true)
            .open(&main)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(
            Cache::with_tool(dir.path(), tool.clone(), std::env::consts::OS)
                .unwrap()
                .read()
                .is_none()
        );
        fs::write(&main, &source).unwrap();
        let added = dir.path().join("src/added.aside");
        fs::write(&added, "pub fn f() { 1 }").unwrap();
        assert!(
            Cache::with_tool(dir.path(), tool.clone(), std::env::consts::OS)
                .unwrap()
                .read()
                .is_none()
        );
        fs::remove_file(added).unwrap();
        assert!(
            Cache::with_tool(dir.path(), tool.clone(), std::env::consts::OS)
                .unwrap()
                .read()
                .is_some()
        );
        fs::write(&tool, "compiler two").unwrap();
        assert!(
            Cache::with_tool(dir.path(), tool.clone(), std::env::consts::OS)
                .unwrap()
                .read()
                .is_none()
        );
        fs::write(&tool, "compiler one").unwrap();
        fs::write(dir.path().join("build/dev/game.asb"), "interrupted cache").unwrap();
        assert!(cache.read().is_none());
        cache.store(&original).unwrap();
        assert!(cache.read().is_some());
    }
}

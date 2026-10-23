//! Build and load a relocatable author export through the ordinary ASB/AssetIo path.
use crate::{Artifact, BuildProfile, project::Loaded};
use aside_vm::bytecode::TextPool;
use reel_package::{
    archive::{ArchiveIndex, ArchiveKind, ArchiveWriter},
    distribution::{Distribution, LooseResources, relative_path},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
};

pub const TEXT_POOL_ENTRY: &str = "__reel/text.pool";
pub const MAIN_PACKAGE: &str = "game.asb";

/// Add a prepared language index without changing bytecode or code identity.
/// The content-addressed archive is already published beside the target ASB.
pub fn with_catalogs(
    artifact: &Artifact,
    prepared: &crate::localization::PreparedCatalogs,
) -> io::Result<Vec<u8>> {
    let Some(archive) = &prepared.archive else {
        return Ok(artifact.bytes.clone());
    };
    let manifest = Distribution {
        text: Some(
            archive
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| io::Error::other("catalog archive filename must be UTF-8"))?
                .into(),
        ),
        resources: Vec::new(),
        catalogs: prepared.catalogs.clone(),
        loose: Some(LooseResources::Project),
    };
    let limits = reel_package::Limits::default();
    let package = reel_package::decode(&artifact.bytes, limits).map_err(io::Error::other)?;
    let bytecode = aside_vm::bytecode::encode(package.program().package(), limits.bytecode)
        .map_err(io::Error::other)?;
    reel_package::encode(&bytecode, package.metadata(), Some(&manifest), limits)
        .map_err(io::Error::other)
}

pub fn player_path(cli: &Path) -> PathBuf {
    cli.with_file_name(format!("reel-game{}", std::env::consts::EXE_SUFFIX))
}

pub fn exported_player_name() -> String {
    format!("game{}", std::env::consts::EXE_SUFFIX)
}

#[derive(Clone, Debug)]
pub struct Mount {
    pub prefix: String,
    pub directory: PathBuf,
}
impl Mount {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (prefix, directory) = value.split_once('=').ok_or("expected prefix=directory")?;
        if prefix != "." {
            relative_path(prefix).map_err(|e| e.to_string())?;
        }
        if prefix.starts_with("__reel") || directory.is_empty() {
            return Err("use a resource prefix outside __reel and a nonempty directory".into());
        }
        Ok(Self {
            prefix: prefix.into(),
            directory: directory.into(),
        })
    }
}

pub struct ExportOptions {
    pub output: PathBuf,
    pub overwrite: bool,
    pub resources: Vec<Mount>,
    pub text: Vec<Mount>,
    pub pack_bytes: NonZeroU64,
    /// Platform player to copy into a release export.
    pub player: Option<PathBuf>,
}

fn resource_volumes(
    files: &BTreeMap<String, PathBuf>,
    staging: &Path,
    target: u64,
) -> io::Result<Vec<String>> {
    let mut names = vec!["resources.rpk".to_owned()];
    let mut writer = ArchiveWriter::new(
        File::create(staging.join(&names[0]))?,
        ArchiveKind::Resources,
    )?;
    for (name, path) in files {
        let mut file = File::open(path)?;
        if !writer.is_empty() && writer.encoded_len_after(name, file.metadata()?.len())? > target {
            writer.finish()?.sync_all()?;
            let volume = format!("resources-{:03}.rpk", names.len() + 1);
            writer =
                ArchiveWriter::new(File::create(staging.join(&volume))?, ArchiveKind::Resources)?;
            names.push(volume);
        }
        writer.add(name, &mut file)?;
    }
    writer.finish()?.sync_all()?;
    Ok(names)
}

pub struct Exported {
    pub files: usize,
    pub bytes: u64,
}

/// Collect names only. Large media payloads never enter the compiler's memory.
fn collect(root: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> io::Result<()> {
    fn walk(
        root: &Path,
        directory: &Path,
        prefix: &str,
        out: &mut BTreeMap<String, PathBuf>,
    ) -> io::Result<()> {
        for item in fs::read_dir(directory)? {
            let item = item?;
            let ty = item.file_type()?;
            if ty.is_dir() {
                walk(root, &item.path(), prefix, out)?;
            } else if ty.is_file() {
                let file = item.path();
                let suffix = file
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .ok_or_else(|| io::Error::other("resource path must be UTF-8"))?
                    .replace('\\', "/");
                let name = if prefix == "." {
                    suffix
                } else {
                    format!("{prefix}/{suffix}")
                };
                relative_path(&name)?;
                if out.insert(name.clone(), file).is_some() {
                    return Err(io::Error::other(format!(
                        "duplicate exported resource: {name}"
                    )));
                }
            } else {
                return Err(io::Error::other(format!(
                    "export requires regular files: {}",
                    item.path().display()
                )));
            }
        }
        Ok(())
    }
    walk(root, root, prefix, out)
}

fn copy(file: &Path, target: &Path) -> io::Result<u64> {
    fs::create_dir_all(target.parent().unwrap())?;
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "exported source/resource paths overlap: {}",
                target.display()
            ),
        ));
    }
    fs::copy(file, target)
}

fn sources(loaded: &Loaded, staging: &Path) -> io::Result<()> {
    let root = loaded.root.canonicalize()?;
    fs::create_dir_all(staging.join("src"))?;
    for file in &loaded.files {
        let file = file.canonicalize()?;
        let relative = file.strip_prefix(&root).map_err(|_| {
            io::Error::other(format!("source outside project src: {}", file.display()))
        })?;
        copy(&file, &staging.join("src").join(relative))?;
    }
    Ok(())
}

pub fn export(
    loaded: &Loaded,
    artifact: &Artifact,
    options: &ExportOptions,
) -> io::Result<Exported> {
    let parent = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = options
        .output
        .file_name()
        .ok_or_else(|| io::Error::other("export output needs a directory name"))?;
    let requested_output = parent.join(name);
    let output = match fs::symlink_metadata(&requested_output) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::other("export output must not be a link"));
        }
        Ok(_) => requested_output.canonicalize()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => requested_output,
        Err(error) => return Err(error),
    };
    for file in &loaded.files {
        if file.canonicalize()?.starts_with(&output) {
            return Err(io::Error::other(
                "export output contains an input source file",
            ));
        }
    }
    if output.exists() {
        if !options.overwrite {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "export exists; pass --overwrite to replace it",
            ));
        }
        read_manifest(&output.join(MAIN_PACKAGE))?;
    }
    let mut resources = BTreeMap::new();
    let mut text = BTreeMap::new();
    for (mounts, files) in [
        (&options.resources, &mut resources),
        (&options.text, &mut text),
    ] {
        for mount in mounts {
            let root = mount.directory.canonicalize()?;
            if root.starts_with(&output) || output.starts_with(&root) {
                return Err(io::Error::other(
                    "export output overlaps a resource directory",
                ));
            }
            collect(&root, &mount.prefix, files)?;
        }
    }
    let mut names = BTreeSet::new();
    for name in resources.keys().chain(text.keys()) {
        if !names.insert(name) {
            return Err(io::Error::other(format!(
                "text and resources both define {name}"
            )));
        }
    }
    let catalogs = crate::localization::bundle_project(&loaded.project_root, &artifact.catalogs)?;
    for key in catalogs.files.keys() {
        if resources.contains_key(key) || text.contains_key(key) {
            return Err(io::Error::other(format!(
                "project language catalog conflicts with an explicit resource or text mount: {key}"
            )));
        }
    }
    let player = if artifact.profile == BuildProfile::Release {
        Some(
            options
                .player
                .as_deref()
                .ok_or_else(|| io::Error::other("release export requires the platform player"))?,
        )
    } else {
        None
    };
    let candidate = tempfile::Builder::new()
        .prefix(".reel-export-")
        .tempdir_in(&parent)?;
    let staging = candidate.path();
    let result: io::Result<Exported> = (|| {
        let mut manifest = Distribution {
            text: None,
            resources: vec![],
            catalogs: catalogs.catalogs.clone(),
            loose: None,
        };
        if artifact.profile == BuildProfile::Release {
            let pool = artifact
                .text_pool
                .as_ref()
                .ok_or_else(|| io::Error::other("release export requires external text"))?;
            let mut writer =
                ArchiveWriter::new(File::create(staging.join("game.rtp"))?, ArchiveKind::Text)?;
            writer.add(TEXT_POOL_ENTRY, &mut pool.as_bytes())?;
            for (name, file) in &text {
                writer.add(name, &mut File::open(file)?)?;
            }
            for (name, text) in &catalogs.files {
                writer.add(name, &mut text.as_bytes())?;
            }
            writer.finish()?.sync_all()?;
            manifest.text = Some("game.rtp".into());
            manifest.resources = resource_volumes(&resources, staging, options.pack_bytes.get())?;
        } else {
            fs::create_dir_all(staging.join("resources"))?;
            if !catalogs.files.is_empty() {
                let mut writer = ArchiveWriter::new(
                    File::create_new(staging.join("game.rtp"))?,
                    ArchiveKind::Text,
                )?;
                for (name, text) in &catalogs.files {
                    writer.add(name, &mut text.as_bytes())?;
                }
                writer.finish()?.sync_all()?;
                manifest.text = Some("game.rtp".into());
            }
            for (name, file) in resources.iter().chain(text.iter()) {
                copy(file, &staging.join("resources").join(name))?;
            }
            manifest.loose = Some(LooseResources::Directory("resources".into()));
            sources(loaded, staging)?;
        }
        manifest.validate()?;
        let limits = reel_package::Limits::default();
        let package = reel_package::decode(&artifact.bytes, limits).map_err(io::Error::other)?;
        let bytecode = aside_vm::bytecode::encode(package.program().package(), limits.bytecode)
            .map_err(io::Error::other)?;
        let encoded = reel_package::encode(&bytecode, package.metadata(), Some(&manifest), limits)
            .map_err(io::Error::other)?;
        File::create_new(staging.join(MAIN_PACKAGE))?.write_all(&encoded)?;
        let player_bytes = if let Some(player) = player {
            fs::copy(player, staging.join(exported_player_name()))?
        } else {
            0
        };
        let bytes = encoded.len() as u64
            + player_bytes
            + resources
                .values()
                .chain(text.values())
                .try_fold(0u64, |n, path| fs::metadata(path).map(|m| n + m.len()))?
            + catalogs
                .files
                .values()
                .map(|text| text.len() as u64)
                .sum::<u64>()
            + artifact
                .text_pool
                .as_ref()
                .map_or(0, |p| p.as_bytes().len() as u64);
        Ok(Exported {
            files: resources.len() + text.len() + catalogs.files.len(),
            bytes,
        })
    })();
    let result = result?;
    // Build every file before touching the previous export. Both renames stay
    // on the same filesystem; rollback keeps the old output if publication fails.
    let backup = tempfile::Builder::new()
        .prefix(".reel-previous-")
        .tempdir_in(&parent)?;
    let old = backup.path().join("export");
    let replacing = output.exists();
    if replacing && !options.overwrite {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "another export created the output; pass --overwrite to replace it",
        ));
    }
    if replacing {
        read_manifest(&output.join(MAIN_PACKAGE))?;
    }
    if replacing {
        fs::rename(&output, &old)?;
    }
    if let Err(error) = fs::rename(staging, &output) {
        if replacing && let Err(restore) = fs::rename(&old, &output) {
            let retained = backup.keep();
            return Err(io::Error::other(format!(
                "{error}; restoring previous export failed: {restore}; previous export retained at {}",
                retained.join("export").display()
            )));
        }
        return Err(error);
    }
    backup.close()?;
    Ok(result)
}

pub fn read_manifest(path: &Path) -> io::Result<Distribution> {
    reel_package::read_distribution(&mut File::open(path)?, Default::default())?
        .ok_or_else(|| io::Error::other("ASB has no embedded distribution"))
}

pub struct LoadedDistribution {
    pub package: reel_package::CodePackage,
    pub root: PathBuf,
    pub packages: Vec<PathBuf>,
    pub loose: Option<PathBuf>,
    pub catalogs: BTreeMap<String, String>,
}

/// Resource locations resolved once for either an ASB launch or editable export.
pub struct ResourceContext {
    pub packages: Vec<PathBuf>,
    pub loose: Option<PathBuf>,
    pub catalogs: BTreeMap<String, String>,
}

/// Interpret a distribution's package and loose-asset rules without decoding code.
pub fn resource_context(root: &Path, manifest: Distribution) -> io::Result<ResourceContext> {
    let mut packages = Vec::new();
    if let Some(text) = manifest.text {
        packages.push(root.join(text));
    }
    packages.extend(manifest.resources.into_iter().map(|name| root.join(name)));
    let loose = match manifest.loose {
        Some(LooseResources::Directory(directory)) => Some(root.join(directory)),
        Some(LooseResources::Project) => {
            let project = crate::project::find(root)?;
            Some(
                project
                    .map_or_else(|| root.to_path_buf(), |project| project.root)
                    .join("resources"),
            )
        }
        None => None,
    };
    Ok(ResourceContext {
        packages,
        loose,
        catalogs: manifest.catalogs,
    })
}

pub fn load(path: &Path) -> io::Result<LoadedDistribution> {
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let bytes = fs::read(path)?;
    let mut package = reel_package::decode(&bytes, Default::default()).map_err(io::Error::other)?;
    // The decoded package owns its fields; release the ASB before loading text.
    drop(bytes);
    let manifest = package
        .distribution()
        .cloned()
        .unwrap_or_else(|| Distribution {
            text: None,
            resources: vec![],
            catalogs: BTreeMap::new(),
            loose: Some(LooseResources::Project),
        });
    if let Some(text) = &manifest.text {
        let path = root.join(text);
        let mut file = File::open(&path)?;
        let index = ArchiveIndex::read(&mut file)?;
        if index.kind() != ArchiveKind::Text {
            return Err(io::Error::other("text package has resource kind"));
        }
        for (locale, key) in &manifest.catalogs {
            if index.get(key).is_none() {
                return Err(io::Error::other(format!(
                    "text archive lacks indexed catalog {locale}: {key}"
                )));
            }
        }
        if let Some(descriptor) = package.program().package().text_pool {
            let entry = index
                .get(TEXT_POOL_ENTRY)
                .ok_or_else(|| io::Error::other("text package has no original text pool"))?;
            if descriptor.byte_len != entry.length || descriptor.digest != entry.version {
                return Err(io::Error::other("text pool does not match ASB"));
            }
            let len = usize::try_from(entry.length).map_err(io::Error::other)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(len).map_err(io::Error::other)?;
            file.seek(SeekFrom::Start(entry.offset))?;
            file.take(entry.length).read_to_end(&mut bytes)?;
            let pool = TextPool::from_bytes(bytes).map_err(io::Error::other)?;
            package = package.bind_text_pool(pool).map_err(io::Error::other)?;
        }
    } else if package.program().package().text_pool.is_some() {
        return Err(io::Error::other("ASB requires a text package"));
    }
    let context = resource_context(&root, manifest)?;
    Ok(LoadedDistribution {
        package,
        root,
        packages: context.packages,
        loose: context.loose,
        catalogs: context.catalogs,
    })
}

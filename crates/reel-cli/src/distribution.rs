//! Build a relocatable author export through the ordinary ASB/AssetIo path.
use crate::{BuildProfile, PreparedCode, project::Loaded, resource_conversion};
use reel_package::{
    archive::{ArchiveKind, ArchiveWriter},
    distribution::{Distribution, LooseResources, MAIN_PACKAGE, TEXT_POOL_ENTRY, relative_path},
    resource_target::ResourceTarget,
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use reel_player::distribution::read_manifest;

/// Resolve the content-addressed sidecars for a standalone ASB build.
pub fn build_manifest(
    catalogs: &crate::localization::PreparedCatalogs,
    effects: Option<&Path>,
) -> io::Result<Option<Distribution>> {
    if catalogs.archive.is_none() && effects.is_none() {
        return Ok(None);
    }
    let name = |path: &Path| {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("archive filename must be UTF-8"))
    };
    Ok(Some(Distribution {
        text: catalogs.archive.as_deref().map(name).transpose()?,
        resources: effects.map(name).transpose()?.into_iter().collect(),
        catalogs: catalogs.catalogs.clone(),
        loose: Some(LooseResources::Project),
    }))
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

/// Native runtime payload placed beside the exported game package.
pub enum Runtime {
    /// The current tool bundle's player, published as the default game executable.
    Executable(PathBuf),
    /// Another platform's complete runtime, with relative paths preserved.
    Directory(PathBuf),
}
impl Runtime {
    /// Resolve the implicit runtime for a release resource target. Vita uses the
    /// shared launcher. Explicit desktop OS targets require a matching player.
    pub fn default_for_target(target: &str) -> io::Result<Option<Self>> {
        let host = std::env::consts::OS;
        match target {
            "vita" => return Ok(None),
            "windows" | "linux" | "macos" if target != host => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "exporting for {target} from {host} requires --runtime DIRECTORY or --data-only"
                    ),
                ));
            }
            _ => {}
        }
        let player = player_path(&std::env::current_exe()?);
        if !player.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "platform player {} is missing; download the complete Reel tool package, including reel-game",
                    player.display()
                ),
            ));
        }
        Ok(Some(Self::Executable(player)))
    }
}

pub struct ExportOptions {
    pub output: PathBuf,
    pub overwrite: bool,
    pub resources: Vec<Mount>,
    pub text: Vec<Mount>,
    /// Source-resource variants to select before writing either export profile.
    pub resource_target: String,
    pub pack_bytes: NonZeroU64,
    /// Platform runtime to copy into a release export. `None` exports game data
    /// for a shared runtime already installed on the destination device.
    pub runtime: Option<Runtime>,
}

fn resource_volumes(
    files: &BTreeMap<String, PathBuf>,
    staging: &Path,
    target: u64,
) -> io::Result<(Vec<String>, u64)> {
    let mut names = vec!["resources.rpk".to_owned()];
    let mut writer = ArchiveWriter::new(
        File::create(staging.join(&names[0]))?,
        ArchiveKind::Resources,
    )?;
    let mut bytes = 0;
    for (name, path) in files {
        let mut file = File::open(path)?;
        if !writer.is_empty() && writer.encoded_len_after(name, file.metadata()?.len())? > target {
            writer.finish()?.sync_all()?;
            let volume = format!("resources-{:03}.rpk", names.len() + 1);
            writer =
                ArchiveWriter::new(File::create(staging.join(&volume))?, ArchiveKind::Resources)?;
            names.push(volume);
        }
        let entry = if resource_conversion::has_mp4_extension(path)
            && resource_conversion::has_mp4_header(&mut file)?
        {
            // H.264/AAC payloads are already compressed. Contiguous storage
            // lets the player seek without reading an LZ4 block directory.
            writer.add_raw(name, &mut file)?
        } else {
            writer.add(name, &mut file)?
        };
        bytes += entry.length;
    }
    writer.finish()?.sync_all()?;
    Ok((names, bytes))
}

pub struct Exported {
    pub files: usize,
    pub bytes: u64,
}

/// Collect names only. Large media payloads never enter the compiler's memory.
fn collect(root: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> io::Result<()> {
    for item in walkdir::WalkDir::new(root).min_depth(1) {
        let item = item?;
        let ty = item.file_type();
        if ty.is_dir() {
            continue;
        } else if ty.is_file() {
            let file = item.into_path();
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
    artifact: &PreparedCode<'_>,
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
    let target = ResourceTarget::parse(&options.resource_target)?;
    let mut runtime_files = BTreeMap::new();
    if artifact.profile == BuildProfile::Release
        && let Some(runtime) = &options.runtime
    {
        match runtime {
            Runtime::Executable(path) => {
                runtime_files.insert(exported_player_name(), path.clone());
            }
            Runtime::Directory(path) => {
                let root = path.canonicalize()?;
                if root.starts_with(&output) || output.starts_with(&root) {
                    return Err(io::Error::other(
                        "export output overlaps the runtime directory",
                    ));
                }
                collect(&root, ".", &mut runtime_files)?;
                if target.as_str() == "vita" && !runtime_files.contains_key("eboot.bin") {
                    return Err(io::Error::other(
                        "Vita runtime directory requires eboot.bin at its root",
                    ));
                }
                if runtime_files.is_empty() {
                    return Err(io::Error::other("runtime directory contains no files"));
                }
            }
        }
    }
    let mut resources = BTreeMap::new();
    let mut text = BTreeMap::new();
    for (mounts, files, variants) in [
        (&options.resources, &mut resources, true),
        (&options.text, &mut text, false),
    ] {
        for mount in mounts {
            let root = mount.directory.canonicalize()?;
            if root.starts_with(&output) || output.starts_with(&root) {
                return Err(io::Error::other(
                    "export output overlaps a resource directory",
                ));
            }
            if variants {
                for (suffix, path) in target.collect(&root)? {
                    let name = if mount.prefix == "." {
                        suffix
                    } else {
                        format!("{}/{suffix}", mount.prefix)
                    };
                    relative_path(&name)?;
                    if files.insert(name.clone(), path).is_some() {
                        return Err(io::Error::other(format!(
                            "duplicate exported resource: {name}"
                        )));
                    }
                }
            } else {
                collect(&root, &mount.prefix, files)?;
            }
        }
    }
    let mut resources =
        crate::resource_conversion::prepare_selected(&loaded.project_root, &target, resources)?
            .files;
    crate::prepare_effects::merge(
        &mut resources,
        crate::prepare_effects::prepare(
            &loaded.project_root,
            &artifact.effects,
            crate::prepare_effects::Target::for_export(target.as_str()),
        )?,
    )?;
    if resources.values().any(|path| path.starts_with(&output)) {
        return Err(io::Error::other(
            "export output contains a prepared resource input",
        ));
    }
    if let Some(name) = text.keys().find(|name| resources.contains_key(*name)) {
        return Err(io::Error::other(format!(
            "text and resources both define {name}"
        )));
    }
    let catalogs = crate::localization::bundle_project(&loaded.project_root, &artifact.catalogs)?;
    for key in catalogs.files.keys() {
        if resources.contains_key(key) || text.contains_key(key) {
            return Err(io::Error::other(format!(
                "project language catalog conflicts with an explicit resource or text mount: {key}"
            )));
        }
    }
    let candidate = tempfile::Builder::new()
        .prefix(".reel-export-")
        .tempdir_in(&parent)?;
    let staging = candidate.path();
    let result = {
        let mut manifest = Distribution {
            text: None,
            resources: vec![],
            catalogs: catalogs.catalogs,
            loose: None,
        };
        let mut asset_bytes = 0;
        let release = artifact.profile == BuildProfile::Release;
        if release || !catalogs.files.is_empty() {
            let mut writer = ArchiveWriter::new(
                File::create_new(staging.join("game.rtp"))?,
                ArchiveKind::Text,
            )?;
            if release {
                let pool = artifact
                    .text_pool
                    .as_ref()
                    .ok_or_else(|| io::Error::other("release export requires external text"))?;
                asset_bytes += writer.add(TEXT_POOL_ENTRY, &mut pool.as_bytes())?.length;
                for (name, file) in &text {
                    asset_bytes += writer.add(name, &mut File::open(file)?)?.length;
                }
            }
            for (name, text) in &catalogs.files {
                asset_bytes += writer.add(name, &mut text.as_bytes())?.length;
            }
            writer.finish()?.sync_all()?;
            manifest.text = Some("game.rtp".into());
        }
        if release {
            let (volumes, bytes) = resource_volumes(&resources, staging, options.pack_bytes.get())?;
            manifest.resources = volumes;
            asset_bytes += bytes;
        } else {
            fs::create_dir_all(staging.join("resources"))?;
            for (name, file) in resources.iter().chain(text.iter()) {
                asset_bytes += copy(file, &staging.join("resources").join(name))?;
            }
            manifest.loose = Some(LooseResources::Directory("resources".into()));
            sources(loaded, staging)?;
        }
        let encoded = artifact.encode(Some(&manifest)).map_err(io::Error::other)?;
        File::create_new(staging.join(MAIN_PACKAGE))?.write_all(&encoded)?;
        let runtime_bytes = runtime_files.iter().try_fold(0u64, |total, (name, file)| {
            copy(file, &staging.join(name)).map(|bytes| total + bytes)
        })?;
        Exported {
            files: resources.len() + text.len() + catalogs.files.len(),
            bytes: encoded.len() as u64 + runtime_bytes + asset_bytes,
        }
    };
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

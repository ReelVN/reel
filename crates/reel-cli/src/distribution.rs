//! Build and load a relocatable author export through the ordinary ASB/AssetIo path.
use crate::{Artifact, BuildProfile, project::Loaded};
use aside_vm::bytecode::TextPool;
use reel_package::{archive::{ArchiveIndex, ArchiveKind, ArchiveWriter}, distribution::{Distribution, SourceProject, relative_path}};
use std::{collections::{BTreeMap, BTreeSet}, fs::{self, File}, io::{self, Read, Seek, SeekFrom}, path::{Path, PathBuf}, sync::Arc};

pub const TEXT_POOL_ENTRY: &str = "__reel/text.pool";

pub struct Mount {
    pub prefix: String,
    pub directory: PathBuf,
}
impl Mount {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (prefix, directory) = value.split_once('=').ok_or("expected prefix=directory")?;
        relative_path(prefix).map_err(|e| e.to_string())?;
        if prefix == "." || prefix.starts_with("__reel") || directory.is_empty() {
            return Err("use a nonempty resource prefix outside __reel".into());
        }
        Ok(Self { prefix: prefix.into(), directory: directory.into() })
    }
}

pub struct ExportOptions {
    pub output: PathBuf,
    pub overwrite: bool,
    pub resources: Vec<Mount>,
    pub text: Vec<Mount>,
    pub modules: BTreeMap<String, PathBuf>,
    pub entry: String,
}

pub struct Exported {
    pub files: usize,
    pub bytes: u64,
}

/// Collect names only. Large media payloads never enter the compiler's memory.
fn collect(root: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> io::Result<()> {
    fn walk(root: &Path, directory: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> io::Result<()> {
        for item in fs::read_dir(directory)? {
            let item = item?;
            let ty = item.file_type()?;
            if ty.is_dir() { walk(root, &item.path(), prefix, out)?; }
            else if ty.is_file() {
                let file = item.path();
                let suffix = file.strip_prefix(root).unwrap().to_str()
                    .ok_or_else(|| io::Error::other("resource path must be UTF-8"))?.replace('\\', "/");
                let name = format!("{prefix}/{suffix}");
                relative_path(&name)?;
                if out.insert(name.clone(), file).is_some() {
                    return Err(io::Error::other(format!("duplicate exported resource: {name}")));
                }
            } else {
                return Err(io::Error::other(format!("export requires regular files: {}", item.path().display())));
            }
        }
        Ok(())
    }
    walk(root, root, prefix, out)
}

fn copy(file: &Path, target: &Path) -> io::Result<u64> {
    fs::create_dir_all(target.parent().unwrap())?;
    fs::copy(file, target)
}

fn sources(loaded: &Loaded, options: &ExportOptions, staging: &Path) -> io::Result<SourceProject> {
    let mut roots = vec![(loaded.root.canonicalize()?, "script".to_owned())];
    let mut modules = BTreeMap::new();
    for (alias, directory) in &options.modules {
        let root = directory.canonicalize()?;
        let target = roots.iter().find(|(path, _)| path == &root).map(|(_, name)| name.clone())
            .unwrap_or_else(|| format!("script/_modules/{alias}"));
        relative_path(&target)?;
        modules.insert(alias.clone(), target.clone());
        if !roots.iter().any(|(path, _)| path == &root) { roots.push((root, target)); }
    }
    // Most specific root wins when libraries live under the project directory.
    roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    let mut entry = None;
    let input = loaded.input.canonicalize()?;
    for file in &loaded.files {
        let file = file.canonicalize()?;
        let (root, prefix) = roots.iter().find(|(root, _)| file.starts_with(root))
            .ok_or_else(|| io::Error::other(format!("source outside declared roots: {}", file.display())))?;
        let suffix = file.strip_prefix(root).unwrap().to_str().ok_or_else(|| io::Error::other("source path must be UTF-8"))?.replace('\\', "/");
        let name = format!("{prefix}/{suffix}");
        copy(&file, &staging.join(&name))?;
        if file == input { entry = Some(name); }
    }
    Ok(SourceProject { entry: entry.ok_or_else(|| io::Error::other("entry source missing from project"))?, modules, function: options.entry.clone() })
}

pub fn export(loaded: &Loaded, artifact: &Artifact, options: &ExportOptions) -> io::Result<Exported> {
    let parent = options.output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = options.output.file_name().ok_or_else(|| io::Error::other("export output needs a directory name"))?;
    let output = parent.join(name);
    let source_root = loaded.root.canonicalize()?;
    if output.starts_with(&source_root) || source_root.starts_with(&output) {
        return Err(io::Error::other("export output must be outside the source tree"));
    }
    if output.exists() {
        if !options.overwrite { return Err(io::Error::new(io::ErrorKind::AlreadyExists, "export exists; pass --overwrite to replace it")); }
        if fs::symlink_metadata(&output)?.file_type().is_symlink() { return Err(io::Error::other("export output must not be a link")); }
        read_manifest(&output.join("release.json"))?;
    }
    let mut resources = BTreeMap::new();
    let mut text = BTreeMap::new();
    for (mounts, files) in [(&options.resources, &mut resources), (&options.text, &mut text)] {
        for mount in mounts {
            let root = mount.directory.canonicalize()?;
            if root.starts_with(&output) || output.starts_with(&root) { return Err(io::Error::other("export output overlaps a resource directory")); }
            collect(&root, &mount.prefix, files)?;
        }
    }
    let mut names = BTreeSet::new();
    for name in resources.keys().chain(text.keys()) {
        if !names.insert(name) { return Err(io::Error::other(format!("text and resources both define {name}"))); }
    }
    let staging = parent.join(format!(".{}.export-{}", name.to_string_lossy(), std::process::id()));
    fs::create_dir(&staging)?;
    let result = (|| {
        fs::write(staging.join("game.asb"), &artifact.bytes)?;
        let mut manifest = Distribution { code: "game.asb".into(), text: None, resources: vec![], loose: None, source: None };
        if artifact.profile == BuildProfile::Release {
            let pool = artifact.text_pool.as_ref().ok_or_else(|| io::Error::other("release export requires external text"))?;
            let mut writer = ArchiveWriter::new(File::create(staging.join("game.rtp"))?, ArchiveKind::Text)?;
            writer.add(TEXT_POOL_ENTRY, &mut pool.as_bytes())?;
            for (name, file) in &text { writer.add(name, &mut File::open(file)?)?; }
            writer.finish()?.sync_all()?;
            manifest.text = Some("game.rtp".into());
            let mut writer = ArchiveWriter::new(File::create(staging.join("resources.rpk"))?, ArchiveKind::Resources)?;
            for (name, file) in &resources { writer.add(name, &mut File::open(file)?)?; }
            writer.finish()?.sync_all()?;
            manifest.resources.push("resources.rpk".into());
        } else {
            for (name, file) in resources.iter().chain(text.iter()) { copy(file, &staging.join(name))?; }
            manifest.loose = Some(".".into());
            manifest.source = Some(sources(loaded, options, &staging)?);
        }
        manifest.validate()?;
        fs::write(staging.join("release.json"), serde_json::to_vec_pretty(&manifest)?)?;
        let bytes = artifact.bytes.len() as u64 + resources.values().chain(text.values()).try_fold(0u64, |n, path| fs::metadata(path).map(|m| n + m.len()))?
            + artifact.text_pool.as_ref().map_or(0, |p| p.as_bytes().len() as u64);
        Ok(Exported { files: resources.len() + text.len(), bytes })
    })();
    if result.is_err() { let _ = fs::remove_dir_all(&staging); return result; }
    // Build every file before touching the previous export. Both renames stay
    // on the same filesystem; rollback keeps the old output if publication fails.
    let old = parent.join(format!(".{}.previous-{}", name.to_string_lossy(), std::process::id()));
    let replacing = output.exists();
    if replacing { fs::rename(&output, &old)?; }
    if let Err(error) = fs::rename(&staging, &output) {
        if replacing { let _ = fs::rename(&old, &output); }
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if replacing { fs::remove_dir_all(old)?; }
    result
}

pub fn read_manifest(path: &Path) -> io::Result<Distribution> {
    let manifest: Distribution = serde_json::from_reader(File::open(path)?)?;
    manifest.validate()?;
    Ok(manifest)
}

pub struct LoadedDistribution {
    pub package: reel_package::CodePackage,
    pub root: PathBuf,
    pub packages: Vec<PathBuf>,
    pub loose: Option<PathBuf>,
}

pub fn load(path: &Path) -> io::Result<LoadedDistribution> {
    let manifest = read_manifest(path)?;
    let root = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).canonicalize()?;
    let bytes = fs::read(root.join(&manifest.code))?;
    let mut package = reel_package::decode(&bytes, Default::default()).map_err(io::Error::other)?;
    let mut packages = Vec::new();
    if let Some(text) = &manifest.text {
        let path = root.join(text);
        let mut file = File::open(&path)?;
        let index = ArchiveIndex::read(&mut file)?;
        if index.kind() != ArchiveKind::Text { return Err(io::Error::other("text package has resource kind")); }
        let entry = index.get(TEXT_POOL_ENTRY).ok_or_else(|| io::Error::other("text package has no original text pool"))?;
        let descriptor = package.program().package().text_pool.ok_or_else(|| io::Error::other("ASB does not reference an external text pool"))?;
        if descriptor.byte_len != entry.length || descriptor.digest != entry.version { return Err(io::Error::other("text pool does not match ASB")); }
        let len = usize::try_from(entry.length).map_err(io::Error::other)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(io::Error::other)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        file.take(entry.length).read_to_end(&mut bytes)?;
        let pool = TextPool::from_bytes(Arc::from(bytes)).map_err(io::Error::other)?;
        package = package.bind_text_pool(pool).map_err(io::Error::other)?;
        packages.push(path);
    } else if package.program().package().text_pool.is_some() { return Err(io::Error::other("ASB requires a text package")); }
    packages.extend(manifest.resources.iter().map(|name| root.join(name)));
    let loose = manifest.loose.map(|name| root.join(name));
    Ok(LoadedDistribution { package, root, packages, loose })
}

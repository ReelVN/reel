//! Runtime ASB loading and relocatable resource resolution, shared by platform hosts.
//! Author project discovery and source compilation belong to the CLI.
use aside_vm::bytecode::{self as bc, TextPool};
use reel_engine::aside::source::CompiledStory;
use reel_package::{
    archive::{ArchiveIndex, ArchiveKind},
    distribution::{Distribution, LooseResources, TEXT_POOL_ENTRY},
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Decode and verify an ASB with embedded text and require its declared game entry.
/// Use [`load`] for distributions whose text pool resides in an adjacent archive.
pub fn decode_story(bytes: &[u8]) -> Result<CompiledStory, reel_package::Error> {
    story_from_package(reel_package::decode(
        bytes,
        reel_package::Limits::default(),
    )?)
}
/// Transfer a verified package and its declared initializer and game entry into
/// the story admitted by [`crate::aside_session::AsideSessionUi::from_compiled`].
/// Runtime native admission remains the driver's responsibility.
pub fn story_from_package(
    package: reel_package::CodePackage,
) -> Result<CompiledStory, reel_package::Error> {
    let (program, metadata) = package.into_parts();
    let entry = metadata.game_entry.ok_or_else(|| reel_package::Error {
        kind: reel_package::ErrorKind::Metadata,
        detail: "code package has no game entry".into(),
    })?;
    Ok(CompiledStory {
        program,
        initializer: bc::FunctionId(metadata.initializer.function_id),
        entry: bc::FunctionId(entry.function_id),
        catalogs: Default::default(),
    })
}

/// Read the embedded distribution index without decoding the program.
/// ASBs without an index return an error.
pub fn read_manifest(path: &Path) -> io::Result<Distribution> {
    reel_package::read_distribution(&mut File::open(path)?, Default::default())?
        .ok_or_else(|| io::Error::other("ASB has no embedded distribution"))
}

/// Verified code, bound original text and resolved asset locations for one ASB.
/// Archives retain catalog and resource payloads for on-demand asset reads.
pub struct LoadedDistribution {
    /// Code and entry metadata, with the external text pool bound when required.
    pub package: reel_package::CodePackage,
    /// Canonical directory containing the ASB.
    pub root: PathBuf,
    /// Text archive followed by resource volumes, in declared lookup order.
    pub packages: Vec<PathBuf>,
    /// Optional loose-resource directory; `None` keeps archive-only resolution.
    pub loose: Option<PathBuf>,
    /// Canonical language tags mapped to logical text-archive entries.
    pub catalogs: BTreeMap<String, String>,
}

/// Resource locations resolved once for either an ASB launch or editable export.
pub struct ResourceContext {
    /// Text archive followed by resource volumes, resolved against the ASB root.
    pub packages: Vec<PathBuf>,
    /// Optional loose-resource directory selected by the manifest.
    pub loose: Option<PathBuf>,
    /// Validated language index, with catalog contents left in the text archive.
    pub catalogs: BTreeMap<String, String>,
}

/// Interpret a distribution's package and loose-asset rules without decoding code.
/// The manifest must already have passed [`Distribution::validate`].
/// `project_resources` is an optional resource directory resolved by the embedding
/// author tool. It affects only [`LooseResources::Project`]; without it that mode
/// uses `resources` beside the ASB. This function performs no project discovery.
pub fn resource_context(
    root: &Path,
    manifest: Distribution,
    project_resources: Option<&Path>,
) -> ResourceContext {
    let mut packages = Vec::new();
    if let Some(text) = manifest.text {
        packages.push(root.join(text));
    }
    packages.extend(manifest.resources.into_iter().map(|name| root.join(name)));
    let loose = match manifest.loose {
        Some(LooseResources::Directory(directory)) => Some(root.join(directory)),
        Some(LooseResources::Project) => {
            Some(project_resources.map_or_else(|| root.join("resources"), Path::to_path_buf))
        }
        None => None,
    };
    ResourceContext {
        packages,
        loose,
        catalogs: manifest.catalogs,
    }
}

/// Decode and verify an ASB, bind its external original text, and resolve its
/// resource and catalog index. Resource payloads remain in their archives.
/// `project_resources` follows [`resource_context`]; native distribution hosts
/// can pass `None` to resolve project-mode loose files beside the ASB.
pub fn load(path: &Path, project_resources: Option<&Path>) -> io::Result<LoadedDistribution> {
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
    let context = resource_context(&root, manifest, project_resources);
    Ok(LoadedDistribution {
        package,
        root,
        packages: context.packages,
        loose: context.loose,
        catalogs: context.catalogs,
    })
}

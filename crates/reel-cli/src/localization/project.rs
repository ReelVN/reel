//! Project catalogs use the ordinary Fluent bundler and seekable text archive.
use super::{catalog_files, write_bundle};
use reel_engine::localization::language_id;
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

/// A published immutable language archive and its small runtime lookup index.
#[derive(Default)]
pub struct PreparedCatalogs {
    /// Canonical language tags mapped to text-archive entry keys.
    pub catalogs: BTreeMap<String, String>,
    /// Published content-addressed archive; absent for untranslated projects.
    pub archive: Option<PathBuf>,
}

struct Locale {
    tag: String,
    files: Vec<PathBuf>,
}

fn locales(project: &Path) -> io::Result<Vec<Locale>> {
    let source = project.join("translations");
    let entries = match fs::read_dir(&source) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io::Error::other(format!("{}: {error}", source.display()))),
    };
    let mut locales = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            io::Error::other(format!(
                "{}: language directory must be UTF-8",
                entry.path().display()
            ))
        })?;
        let tag = language_id(name)
            .map_err(|error| io::Error::other(format!("{}: {error}", entry.path().display())))?
            .to_string();
        if name != tag {
            return Err(io::Error::other(format!(
                "{}: language directory must use canonical tag {tag}",
                entry.path().display()
            )));
        }
        let files = catalog_files(&entry.path()).map_err(io::Error::other)?;
        if files.is_empty() {
            return Err(io::Error::other(format!(
                "{}: no .ftl catalogs found",
                entry.path().display()
            )));
        }
        locales.insert(tag.clone(), Locale { tag, files });
    }
    let resources = project.join("resources");
    let resources = if resources.is_dir() {
        resources.as_path()
    } else {
        project
    };
    for locale in locales.keys() {
        let key = format!("__reel/catalogs/{locale}.ftl");
        let conflict = resources.join(&key);
        if conflict.try_exists()? {
            return Err(io::Error::other(format!(
                "{}: resource conflicts with generated project language catalog {key}",
                conflict.display()
            )));
        }
    }
    Ok(locales.into_values().collect())
}

/// Validate every project language without creating generated directories or files.
pub fn check_project(project: &Path) -> io::Result<()> {
    for locale in locales(project)? {
        write_bundle(&locale.files, &mut io::sink())?;
    }
    Ok(())
}

pub(crate) struct ProjectBundles {
    pub catalogs: BTreeMap<String, String>,
    pub files: BTreeMap<String, PathBuf>,
    pub sources: Vec<(PathBuf, PathBuf)>,
    _temporary: Option<tempfile::TempDir>,
}

pub(crate) fn bundle_project(project: &Path) -> io::Result<ProjectBundles> {
    let locales = locales(project)?;
    let temporary = if locales.is_empty() {
        None
    } else {
        Some(tempfile::tempdir()?)
    };
    let mut bundles = ProjectBundles {
        catalogs: BTreeMap::new(),
        files: BTreeMap::new(),
        sources: Vec::new(),
        _temporary: temporary,
    };
    for locale in locales {
        let path = bundles
            ._temporary
            .as_ref()
            .unwrap()
            .path()
            .join(format!("{}.ftl", locale.tag));
        let mut file = File::create(&path)?;
        write_bundle(&locale.files, &mut file)?;
        let key = format!("__reel/catalogs/{}.ftl", locale.tag);
        bundles.catalogs.insert(locale.tag, key.clone());
        bundles.files.insert(key, path);
        for source in locale.files {
            let relative = source
                .strip_prefix(project)
                .expect("project translation source")
                .to_path_buf();
            bundles.sources.push((source, relative));
        }
    }
    Ok(bundles)
}

/// Prepare one content-addressed archive before publishing an ASB that names it.
/// Existing archives remain available to older ASBs; unchanged catalogs reuse
/// the same filename. An untranslated project creates no archive or directory.
pub fn prepare_project(project: &Path, output: &Path) -> io::Result<PreparedCatalogs> {
    let bundles = bundle_project(project)?;
    if bundles.files.is_empty() {
        return Ok(PreparedCatalogs::default());
    }
    fs::create_dir_all(output)?;
    let mut candidate = tempfile::NamedTempFile::new_in(output)?;
    let mut writer = ArchiveWriter::new(candidate.as_file_mut(), ArchiveKind::Text)?;
    let mut identity = b"Reel project catalogs v1\0".to_vec();
    for (key, path) in &bundles.files {
        let entry = writer.add(key, &mut File::open(path)?)?;
        identity.extend_from_slice(&(key.len() as u64).to_le_bytes());
        identity.extend_from_slice(key.as_bytes());
        identity.extend_from_slice(&entry.length.to_le_bytes());
        identity.extend_from_slice(&entry.version);
    }
    writer.finish()?.sync_all()?;
    let archive = output.join(format!("catalogs-{}.rtp", reel_package::digest(&identity)));
    match candidate.persist_noclobber(&archive) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error),
    }
    Ok(PreparedCatalogs {
        catalogs: bundles.catalogs,
        archive: Some(archive),
    })
}

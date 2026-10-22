//! Generated source catalogs use the existing seekable text archive.
use reel_engine::aside::source::CatalogProducts;
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use std::{
    collections::BTreeMap,
    fs, io,
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

/// Check generated catalog asset names before creating any output.
pub fn check_project(project: &Path, products: &CatalogProducts) -> io::Result<()> {
    for locale in products.catalogs.keys() {
        let conflict = project.join("resources").join(catalog_key(locale));
        if conflict.try_exists()? {
            return Err(io::Error::other(format!(
                "{}: resource conflicts with generated source language catalog",
                conflict.display()
            )));
        }
    }
    Ok(())
}
fn catalog_key(locale: &str) -> String {
    format!("__reel/catalogs/{locale}.ftl")
}

pub(crate) struct ProjectBundles<'a> {
    pub catalogs: BTreeMap<String, String>,
    pub files: BTreeMap<String, &'a str>,
}
pub(crate) fn bundle_project<'a>(
    project: &Path,
    products: &'a CatalogProducts,
) -> io::Result<ProjectBundles<'a>> {
    check_project(project, products)?;
    Ok(ProjectBundles {
        catalogs: products
            .catalogs
            .keys()
            .map(|locale| (locale.clone(), catalog_key(locale)))
            .collect(),
        files: products
            .catalogs
            .iter()
            .map(|(locale, text)| (catalog_key(locale), text.as_str()))
            .collect(),
    })
}

/// Publish the same generated catalogs used by source execution. Unchanged
/// products reuse their content-addressed archive and older ASBs retain theirs.
/// A project with no translations creates no archive or generated directory.
pub fn prepare_project(
    project: &Path,
    products: &CatalogProducts,
    output: &Path,
) -> io::Result<PreparedCatalogs> {
    let bundles = bundle_project(project, products)?;
    if bundles.files.is_empty() {
        return Ok(PreparedCatalogs::default());
    }
    fs::create_dir_all(output)?;
    let mut candidate = tempfile::NamedTempFile::new_in(output)?;
    let mut writer = ArchiveWriter::new(candidate.as_file_mut(), ArchiveKind::Text)?;
    let mut identity = b"Reel project catalogs v1\0".to_vec();
    for (key, text) in &bundles.files {
        let entry = writer.add(key, &mut text.as_bytes())?;
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

//! Relocatable build output embedded in ASB. Paths are relative to the ASB directory.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Conventional code-package filename placed beside an exported platform player.
pub const MAIN_PACKAGE: &str = "game.asb";
/// Reserved text-archive entry containing the original bytecode text pool.
pub const TEXT_POOL_ENTRY: &str = "__reel/text.pool";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub text: Option<String>,
    pub resources: Vec<String>,
    /// Canonical language tags mapped to logical entries in the text archive.
    /// Catalog contents remain in the archive and are loaded on demand.
    pub catalogs: BTreeMap<String, String>,
    /// None means package-only resolution, without loose-file probing.
    pub loose: Option<LooseResources>,
}

/// Loose assets selected by a project build or a relocatable export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LooseResources {
    /// Use the surrounding project's `resources`, or `resources` beside the ASB.
    Project,
    /// Resolve a directory relative to the ASB, such as an export's `resources`.
    Directory(String),
}

impl Distribution {
    pub fn validate(&self) -> std::io::Result<()> {
        for path in
            self.text
                .iter()
                .chain(self.resources.iter())
                .chain(self.loose.iter().filter_map(|loose| match loose {
                    LooseResources::Project => None,
                    LooseResources::Directory(path) => Some(path),
                }))
        {
            relative_path(path)?;
        }
        if !self.catalogs.is_empty() && self.text.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "language catalogs require a text archive",
            ));
        }
        for (locale, key) in &self.catalogs {
            let language: unic_langid::LanguageIdentifier = locale.parse().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid catalog language {locale:?}: {error}"),
                )
            })?;
            if language.to_string() != *locale {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("catalog language {locale:?} must use canonical form {language}"),
                ));
            }
            crate::archive::validate_path(key)?;
        }
        Ok(())
    }
}

pub fn relative_path(path: &str) -> std::io::Result<()> {
    if path == "." {
        Ok(())
    } else {
        crate::archive::validate_path(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_index_requires_canonical_tags_and_an_archive_without_embedding_contents() {
        let mut manifest = Distribution {
            text: Some("catalogs.rtp".into()),
            resources: Vec::new(),
            catalogs: BTreeMap::from([("en-US".into(), "__reel/catalogs/en-US.ftl".into())]),
            loose: Some(LooseResources::Project),
        };
        manifest.validate().unwrap();
        let encoded = serde_json::to_vec(&manifest).unwrap();
        assert_eq!(
            serde_json::from_slice::<Distribution>(&encoded).unwrap(),
            manifest
        );
        manifest.catalogs = BTreeMap::from([("en-us".into(), "__reel/catalogs/en-US.ftl".into())]);
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("canonical form en-US")
        );
        manifest.catalogs = BTreeMap::from([("en-US".into(), "../outside.ftl".into())]);
        assert!(manifest.validate().is_err());
        manifest.catalogs = BTreeMap::from([("en-US".into(), "catalog.ftl".into())]);
        manifest.text = None;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("text archive")
        );
        manifest.catalogs.clear();
        manifest.loose = Some(LooseResources::Directory("../resources".into()));
        assert!(manifest.validate().is_err());
    }
}

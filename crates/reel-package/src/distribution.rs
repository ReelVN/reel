//! Relocatable build output. Paths are relative to the manifest directory.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::{Component, Path}};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub code: String,
    pub text: Option<String>,
    pub resources: Vec<String>,
    /// None means package-only resolution, without loose-file probing.
    pub loose: Option<String>,
    pub source: Option<SourceProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProject {
    pub entry: String,
    pub modules: BTreeMap<String, String>,
    pub function: String,
}

impl Distribution {
    pub fn validate(&self) -> std::io::Result<()> {
        for path in std::iter::once(&self.code)
            .chain(self.text.iter())
            .chain(self.resources.iter())
            .chain(self.loose.iter())
            .chain(self.source.iter().flat_map(|s| std::iter::once(&s.entry).chain(s.modules.values())))
        {
            relative_path(path)?;
        }
        Ok(())
    }
}

pub fn relative_path(path: &str) -> std::io::Result<()> {
    if path.is_empty() || path.contains(['\\', ':', '\0'])
        || Path::new(path).components().any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        || path.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
            format!("expected a relative package path: {path}")));
    }
    Ok(())
}

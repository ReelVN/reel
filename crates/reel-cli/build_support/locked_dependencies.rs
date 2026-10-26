//! Dependency identities for offline converters, independent of unrelated packages.
use std::{collections::BTreeSet, io};

#[derive(serde::Deserialize)]
pub struct LockedDependencies {
    #[serde(rename = "package")]
    packages: Vec<Package>,
}

#[derive(serde::Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

impl LockedDependencies {
    pub fn parse(source: &str) -> io::Result<Self> {
        toml::from_str(source).map_err(io::Error::other)
    }

    pub fn fingerprint(&self, roots: &[&str]) -> io::Result<blake3::Hash> {
        let mut selected = BTreeSet::new();
        let mut pending = roots
            .iter()
            .map(|name| self.resolve(name))
            .collect::<io::Result<Vec<_>>>()?;
        while let Some(index) = pending.pop() {
            if selected.insert(index) {
                for dependency in &self.packages[index].dependencies {
                    pending.push(self.resolve(dependency)?);
                }
            }
        }
        let mut packages = selected
            .into_iter()
            .map(|index| &self.packages[index])
            .collect::<Vec<_>>();
        packages.sort_by_key(|package| (&package.name, &package.version, &package.source));
        let mut hash = blake3::Hasher::new();
        for package in packages {
            for value in [
                package.name.as_str(),
                package.version.as_str(),
                package.source.as_deref().unwrap_or_default(),
                package.checksum.as_deref().unwrap_or_default(),
            ] {
                hash.update(&(value.len() as u64).to_le_bytes());
                hash.update(value.as_bytes());
            }
        }
        Ok(hash.finalize())
    }

    fn resolve(&self, reference: &str) -> io::Result<usize> {
        let mut parts = reference.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let version = parts.next();
        let source = parts.next().map(|value| value.trim_matches(['(', ')']));
        let mut matches = self.packages.iter().enumerate().filter(|(_, package)| {
            package.name == name
                && version.is_none_or(|value| package.version == value)
                && source.is_none_or(|value| package.source.as_deref() == Some(value))
        });
        match (matches.next(), matches.next()) {
            (Some((index, _)), None) => Ok(index),
            _ => Err(io::Error::other(format!(
                "locked converter dependency must resolve uniquely: {reference}"
            ))),
        }
    }
}

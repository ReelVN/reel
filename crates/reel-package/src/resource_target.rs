//! Select source resources by logical path before development reads or export.
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

/// A source-resource variant name, independent of the executable's build target.
/// Names start with a lowercase ASCII letter and continue with lowercase ASCII
/// letters, digits, underscores or hyphens. The default is the host OS name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceTarget(String);

impl ResourceTarget {
    /// Validate a target name used by a top-level `@NAME` resource directory.
    pub fn parse(name: &str) -> io::Result<Self> {
        if !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource target must start with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, '_' or '-'",
            ));
        }
        Ok(Self(name.into()))
    }

    /// Return the validated variant name without the directory's `@` prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this resource variant uses the desktop runtime's export contracts.
    pub fn is_desktop(&self) -> bool {
        self.as_str() != "vita"
    }

    /// Shared source overlay and conversion rules below the exact target.
    pub fn fallback(&self) -> Option<&'static str> {
        match self.as_str() {
            "windows" | "linux" | "macos" => Some("desktop"),
            _ => None,
        }
    }

    /// Resolve a logical file directly in its source tree, without copying it.
    /// The exact target takes precedence over the shared overlay and common file.
    /// Top-level `@NAME/` paths belong to selection and cannot be requested as
    /// logical resource paths; nested `@NAME` directories remain ordinary paths.
    pub fn resolve(&self, root: &Path, logical: &str) -> io::Result<PathBuf> {
        validate_logical_path(logical, io::ErrorKind::NotFound)?;
        for name in std::iter::once(self.as_str()).chain(self.fallback()) {
            let variant = root.join(format!("@{name}")).join(logical);
            match fs::metadata(&variant) {
                Ok(metadata) if metadata.is_file() => return Ok(variant),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(root.join(logical))
    }

    /// Collect selected logical names in stable order while retaining source
    /// paths. Only the root's `@NAME` directories are reserved. Other target
    /// directories are skipped; selected files replace exact common paths and
    /// target-only files are included. Payload bytes remain on disk.
    pub fn collect(&self, root: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
        let mut files = BTreeMap::new();
        for item in fs::read_dir(root)? {
            let item = item?;
            let kind = item.file_type()?;
            let path = item.path();
            if kind.is_dir() && item.file_name().as_encoded_bytes().starts_with(b"@") {
                continue;
            }
            collect_entry(root, &path, kind, &mut files)?;
        }
        for name in self.fallback().into_iter().chain([self.as_str()]) {
            let selected = root.join(format!("@{name}"));
            match fs::metadata(&selected) {
                Ok(metadata) if metadata.is_dir() => {
                    collect_directory(&selected, &selected, &mut files)?
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(files)
    }
}

impl Default for ResourceTarget {
    fn default() -> Self {
        Self(std::env::consts::OS.into())
    }
}

fn collect_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, PathBuf>,
) -> io::Result<()> {
    for item in walkdir::WalkDir::new(directory).min_depth(1) {
        let item = item?;
        if item.file_type().is_dir() {
            continue;
        }
        collect_entry(root, item.path(), item.file_type(), files)?;
    }
    Ok(())
}

fn collect_entry(
    root: &Path,
    path: &Path,
    kind: fs::FileType,
    files: &mut BTreeMap<String, PathBuf>,
) -> io::Result<()> {
    if kind.is_dir() {
        return collect_directory(root, path, files);
    }
    if !kind.is_file() {
        return Err(io::Error::other(format!(
            "resource selection requires regular files: {}",
            path.display()
        )));
    }
    let name = path
        .strip_prefix(root)
        .expect("collected resource remains under its root")
        .to_str()
        .ok_or_else(|| io::Error::other("resource path must be UTF-8"))?
        .replace('\\', "/");
    validate_logical_path(&name, io::ErrorKind::InvalidData)?;
    files.insert(name, path.into());
    Ok(())
}

fn validate_logical_path(logical: &str, reserved: io::ErrorKind) -> io::Result<()> {
    crate::archive::validate_path(logical)?;
    if logical
        .split_once('/')
        .is_some_and(|(first, _)| first.starts_with('@'))
    {
        return Err(io::Error::new(
            reserved,
            format!(
                "resource target directories are outside the logical resource namespace: {logical:?}"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names_are_portable_and_default_to_the_host_os() {
        assert_eq!(ResourceTarget::default().as_str(), std::env::consts::OS);
        for target in ["vita", "windows", "linux", "macos", "device_2-arm"] {
            assert_eq!(ResourceTarget::parse(target).unwrap().as_str(), target);
        }
        for target in [
            "",
            "Vita",
            "@vita",
            "../vita",
            "vita/foo",
            "vita\\foo",
            "vita.1",
            "设备",
            "1vita",
        ] {
            assert!(ResourceTarget::parse(target).is_err(), "{target}");
        }
    }
}

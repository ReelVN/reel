//! Author metadata and platform shells for standalone desktop releases.
use super::{Exported, relative_path};
use image::{DynamicImage, ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

mod linux;
mod macos;
mod windows;

const IDENTITY_FILE: &str = "game.release.toml";
const MACOS_RESOURCES: &str = "Contents/Resources";
const MACOS_EXECUTABLES: &str = "Contents/MacOS";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Windows,
    Linux,
    Macos,
}

impl Target {
    pub(super) fn resolve(resource_target: &str) -> Option<Self> {
        match resource_target {
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            "vita" => None,
            _ => match env::consts::OS {
                "windows" => Some(Self::Windows),
                "linux" => Some(Self::Linux),
                "macos" => Some(Self::Macos),
                _ => None,
            },
        }
    }

    pub(super) fn executable(self) -> &'static str {
        match self {
            Self::Windows => "game.exe",
            Self::Linux | Self::Macos => "game",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub id: String,
    pub title: String,
}

impl Identity {
    fn validate(&self) -> io::Result<()> {
        let valid_id = self.id.contains('.')
            && self.id.split('.').all(|part| {
                part.starts_with(|ch: char| ch.is_ascii_lowercase())
                    && part.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                    && !part.ends_with('-')
            });
        if !valid_id {
            return Err(io::Error::other(
                "[desktop].id needs a lowercase reverse-domain name, such as com.example.mygame",
            ));
        }
        if self.title.trim().is_empty() || self.title.chars().any(char::is_control) {
            return Err(io::Error::other("[desktop].title needs a display name"));
        }
        Ok(())
    }

    pub(crate) fn read_release(root: &Path) -> io::Result<Option<Self>> {
        let Some(source) = read_optional(&root.join(IDENTITY_FILE))? else {
            return Ok(None);
        };
        let identity: Self = toml::from_str(&source).map_err(io::Error::other)?;
        identity.validate()?;
        Ok(Some(identity))
    }

    pub(crate) fn saves_path(&self) -> io::Result<PathBuf> {
        let absolute_env = |name| {
            env::var_os(name)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let home = || absolute_env("HOME");
        let base = match env::consts::OS {
            "windows" => absolute_env("LOCALAPPDATA"),
            "macos" => home().map(|path| path.join("Library/Application Support")),
            _ => absolute_env("XDG_DATA_HOME")
                .or_else(|| home().map(|path| path.join(".local/share"))),
        }
        .ok_or_else(|| {
            io::Error::other(
                "could not find this user's application data directory; pass --save-directory",
            )
        })?;
        Ok(base.join(&self.id).join("saves"))
    }
}

pub(crate) fn data_directory(executable: &Path) -> io::Result<PathBuf> {
    let root = executable
        .parent()
        .ok_or_else(|| io::Error::other("player executable has no parent directory"))?;
    if cfg!(target_os = "macos") && root.ends_with(MACOS_EXECUTABLES) {
        return Ok(root.with_file_name("Resources"));
    }
    Ok(root.to_owned())
}

#[derive(Deserialize)]
struct ProjectDistribution {
    desktop: Option<DesktopConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopConfig {
    id: String,
    title: String,
    icon: Option<String>,
}

pub(super) struct Release {
    target: Target,
    identity: Identity,
    icon: Option<PathBuf>,
}

impl Release {
    pub(super) fn read(project: &Path, target: Target) -> io::Result<Option<Self>> {
        let path = project.join("distribution.toml");
        let Some(source) = read_optional(&path)? else {
            return Ok(None);
        };
        let config: ProjectDistribution = toml::from_str(&source)
            .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?;
        let Some(config) = config.desktop else {
            return Ok(None);
        };
        let identity = Identity {
            id: config.id,
            title: config.title,
        };
        identity.validate()?;
        let icon = config
            .icon
            .map(|relative| {
                relative_path(&relative)?;
                let path = project.join(relative).canonicalize()?;
                if !path.starts_with(project) {
                    return Err(io::Error::other(
                        "the desktop icon must be inside the project",
                    ));
                }
                Ok(path)
            })
            .transpose()?;
        Ok(Some(Self {
            target,
            identity,
            icon,
        }))
    }

    pub(super) fn output(&self, directory: &Path) -> PathBuf {
        if self.is_bundle() && directory.extension().and_then(|ext| ext.to_str()) != Some("app") {
            // The display name remains verbatim in Info.plist.
            let label = self
                .identity
                .title
                .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-");
            return directory.join(format!("{}.app", label.trim()));
        }
        directory.to_owned()
    }

    pub(super) fn data_path(&self) -> &Path {
        Path::new(if self.is_bundle() {
            MACOS_RESOURCES
        } else {
            ""
        })
    }

    pub(super) fn runtime_path(&self) -> &Path {
        Path::new(if self.is_bundle() {
            MACOS_EXECUTABLES
        } else {
            ""
        })
    }

    fn is_bundle(&self) -> bool {
        self.target == Target::Macos
    }

    pub(super) fn load_icon(&self, output: &Path) -> io::Result<Option<DynamicImage>> {
        let Some(path) = &self.icon else {
            return Ok(None);
        };
        if path.starts_with(output) {
            return Err(io::Error::other(
                "export output contains the desktop icon input",
            ));
        }
        let mut reader = ImageReader::open(path)?;
        reader.set_format(ImageFormat::Png);
        let image = reader.decode().map_err(|error| {
            io::Error::other(format!("desktop icon {}: {error}", path.display()))
        })?;
        if image.width() != image.height() {
            return Err(io::Error::other(format!(
                "desktop icon {} must be square",
                path.display()
            )));
        }
        Ok(Some(image))
    }

    pub(super) fn write(
        &self,
        staging: &Path,
        icon: Option<DynamicImage>,
        result: &mut Exported,
    ) -> io::Result<()> {
        let executable = staging
            .join(self.runtime_path())
            .join(self.target.executable());
        if !executable.is_file() {
            return Err(io::Error::other(format!(
                "desktop runtime needs {} at its root",
                self.target.executable()
            )));
        }
        // Collisions with a custom runtime are errors, as with game.asb and resource volumes.
        let identity = toml::to_string(&self.identity).map_err(io::Error::other)?;
        fs::File::create_new(staging.join(self.data_path()).join(IDENTITY_FILE))?
            .write_all(identity.as_bytes())?;
        result.bytes += identity.len() as u64;
        result.bytes += match self.target {
            Target::Windows => {
                if let Some(icon) = icon {
                    result.bytes -= executable.metadata()?.len();
                    windows::write_icon(&executable, icon)?;
                    executable.metadata()?.len()
                } else {
                    0
                }
            }
            Target::Linux => linux::write_menu(staging, &self.identity, icon)?,
            Target::Macos => macos::write_bundle(staging, &self.identity, icon)?,
        };
        Ok(())
    }
}

fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        )),
    }
}

//! Package a Vita application and publish its game data alongside it.
use super::{
    ExportOptions, Exported, MAIN_PACKAGE, Runtime, collect, copy, export_directory,
    publish_directory, relative_path, stage_export,
};
use crate::{BuildProfile, PreparedCode, project::Loaded};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
struct Config {
    vita: Vita,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vita {
    title_id: String,
    title: String,
    version: String,
    assets: Option<String>,
}

impl Vita {
    fn read(project: &Path) -> io::Result<Self> {
        let path = project.join("distribution.toml");
        let source = fs::read_to_string(&path).map_err(|error| {
            io::Error::new(error.kind(), format!("{}: {error}", path.display()))
        })?;
        let Config { vita } = toml::from_str(&source)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        if vita.title_id.len() != 9
            || !vita
                .title_id
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(invalid(
                "distribution.toml [vita].title_id requires nine uppercase letters or digits",
            ));
        }
        // mksfoex fills both TITLE and the 52-byte STITLE field from this argument.
        if vita.title.trim().is_empty() || vita.title.len() > 51 || vita.title.contains('\0') {
            return Err(invalid(
                "distribution.toml [vita].title needs a nonempty display name up to 51 UTF-8 bytes",
            ));
        }
        match vita.version.as_bytes() {
            [a, b, b'.', c, d] if [a, b, c, d].iter().all(|byte| byte.is_ascii_digit()) => {}
            _ => {
                return Err(invalid(
                    "distribution.toml [vita].version must have the form 01.00",
                ));
            }
        }
        Ok(vita)
    }

    fn system_files(
        &self,
        project: &Path,
        runtime: Option<&Path>,
        output: &Path,
    ) -> io::Result<BTreeMap<String, PathBuf>> {
        let mut files = BTreeMap::new();
        if let Some(defaults) = runtime
            .map(|path| path.join("sce_sys"))
            .filter(|path| path.is_dir())
        {
            collect(&defaults, "sce_sys", &mut files)?;
        }
        if let Some(relative) = &self.assets {
            relative_path(relative)?;
            let assets = project.join(relative).canonicalize()?;
            if !assets.starts_with(project) {
                return Err(invalid("Vita launch assets must be inside the project"));
            }
            if assets.starts_with(output) || output.starts_with(&assets) {
                return Err(invalid("export output overlaps the Vita launch assets"));
            }
            let mut overrides = BTreeMap::new();
            collect(&assets.join("sce_sys"), "sce_sys", &mut overrides)?;
            files.extend(overrides);
        }
        files.remove("sce_sys/param.sfo");
        Ok(files)
    }
}

/// Publish the Vita installer and game data in one release directory.
pub fn export_vita(
    loaded: &Loaded,
    artifact: &PreparedCode<'_>,
    options: &ExportOptions,
    explicit_output: Option<&Path>,
) -> io::Result<(PathBuf, Exported)> {
    if artifact.profile != BuildProfile::Release || options.resource_target != "vita" {
        return Err(invalid("Vita distribution requires a Vita release export"));
    }
    let project = &loaded.project_root;
    let metadata = Vita::read(project)?;
    let data = Path::new("data").join(&metadata.title_id);
    let manifest = data.join(MAIN_PACKAGE);
    let requested = explicit_output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.join("dist/vita").join(&metadata.title_id));
    let output = export_directory(loaded, &requested, &manifest, options.overwrite)?;

    let (eboot, runtime) = match &options.runtime {
        Some(Runtime::Directory(directory)) => {
            let directory = directory.canonicalize()?;
            if directory.starts_with(&output) || output.starts_with(&directory) {
                return Err(invalid("export output overlaps the runtime directory"));
            }
            (directory.join("eboot.bin"), Some(directory))
        }
        Some(Runtime::Executable(path)) => (path.clone(), None),
        None => {
            return Err(invalid(
                "VPK export requires a Vita SELF or runtime directory",
            ));
        }
    };
    if !eboot.is_file() {
        return Err(invalid(format!(
            "Vita runtime is missing {}",
            eboot.display()
        )));
    }
    let eboot = eboot.canonicalize()?;
    if eboot.starts_with(&output) {
        return Err(invalid("export output contains the Vita SELF input"));
    }
    let mut signature = [0; 4];
    fs::File::open(&eboot)?
        .read_exact(&mut signature)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not read Vita SELF at {}: {error}", eboot.display()),
            )
        })?;
    if signature != *b"SCE\0" {
        return Err(invalid(format!(
            "expected a Vita SELF at {}",
            eboot.display()
        )));
    }
    let files = metadata.system_files(project, runtime.as_deref(), &output)?;

    let candidate = tempfile::Builder::new()
        .prefix(".reel-vpk-")
        .tempdir_in(output.parent().unwrap())?;
    let staging = candidate.path();
    let runtime_bytes = pack_vpk(&metadata, &eboot, files, staging)?;
    let mut result = stage_export(
        loaded,
        artifact,
        options,
        None,
        &output,
        &staging.join(data),
    )?;
    result.bytes += runtime_bytes;
    publish_directory(staging, &output, &manifest, options.overwrite)?;
    Ok((requested, result))
}

fn pack_vpk(
    metadata: &Vita,
    eboot: &Path,
    files: BTreeMap<String, PathBuf>,
    release: &Path,
) -> io::Result<u64> {
    let work = tempfile::Builder::new()
        .prefix(".reel-pack-")
        .tempdir_in(release)?;
    let directory = work.path();
    // Use relative inputs for the SDK packer, which does not accept all Windows
    // extended-length path spellings.
    let mut runtime_bytes = fs::copy(eboot, directory.join("eboot.bin"))?;
    for (name, file) in files {
        runtime_bytes += copy(&file, &directory.join(name))?;
    }

    let mut command = tool("vita-mksfoex")?;
    command.current_dir(directory).args([
        "-d",
        "ATTRIBUTE2=12",
        "-s",
        &format!("APP_VER={}", metadata.version),
        "-s",
        &format!("VERSION={}", metadata.version),
        "-s",
        &format!("TITLE_ID={}", metadata.title_id),
        "--",
        &metadata.title,
        "param.sfo",
    ]);
    run(command, "vita-mksfoex")?;
    let mut pack = tool("vita-pack-vpk")?;
    pack.current_dir(directory)
        .args(["-s", "param.sfo", "-b", "eboot.bin"]);
    if directory.join("sce_sys").is_dir() {
        pack.args(["--add", "sce_sys=sce_sys"]);
    }
    let name = format!("{}.vpk", metadata.title_id);
    let pending = directory.join(&name);
    pack.arg(&name);
    run(pack, "vita-pack-vpk")?;
    let mut archive = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pending)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("VitaSDK did not produce {}: {error}", pending.display()),
            )
        })?;
    let mut header = [0; 4];
    archive.read_exact(&mut header).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("VitaSDK did not produce a complete VPK: {error}"),
        )
    })?;
    if header != *b"PK\x03\x04" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VitaSDK produced an invalid VPK",
        ));
    }
    archive.sync_all()?;
    drop(archive);
    fs::rename(pending, release.join(name))?;
    work.close()?;
    Ok(runtime_bytes)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn tool(name: &str) -> io::Result<Command> {
    let executable = format!("{name}{}", env::consts::EXE_SUFFIX);
    let path = env::var_os("VITASDK").map(|root| PathBuf::from(root).join("bin").join(&executable));
    match path {
        Some(path) if path.is_file() => Ok(Command::new(path.canonicalize()?)),
        _ => Ok(Command::new(executable)),
    }
}

fn run(mut command: Command, name: &str) -> io::Result<()> {
    let output = command.output().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not run {name}: {error}. Set VITASDK or add its bin directory to PATH"),
        )
    })?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "{name} exited with {}: {}",
            output.status,
            message.trim()
        )));
    }
    Ok(())
}

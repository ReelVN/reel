//! Shared offline FFmpeg discovery and bounded metadata/error capture.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::OnceLock,
};

pub(crate) struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub identity: String,
}

impl Tools {
    pub fn discover() -> io::Result<&'static Self> {
        static TOOLS: OnceLock<Tools> = OnceLock::new();
        if let Some(tools) = TOOLS.get() {
            return Ok(tools);
        }
        let ffmpeg = executable("ffmpeg")?;
        let ffprobe = executable("ffprobe")?;
        let mut identity = String::new();
        for path in [&ffmpeg, &ffprobe] {
            let mut hasher = blake3::Hasher::new();
            let mut file = File::open(path)?;
            let mut buffer = [0; 65536];
            loop {
                let length = file.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                hasher.update(&buffer[..length]);
            }
            let version = run(command(path).arg("-version"), "media tool version")?;
            identity.push_str(&format!(
                "{}\n{}\n{}\n",
                path.display(),
                hasher.finalize(),
                String::from_utf8_lossy(&version)
            ));
        }
        Ok(TOOLS.get_or_init(|| Self {
            ffmpeg,
            ffprobe,
            identity,
        }))
    }
}

pub(crate) fn command(tool: &Path) -> Command {
    let mut command = Command::new(tool);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

// Files prevent malformed media from growing logs or metadata in memory.
pub(crate) fn run(command: &mut Command, operation: &str) -> io::Result<Vec<u8>> {
    let stdout = run_to_file(command, operation)?;
    if stdout.metadata()?.len() > 1 << 20 {
        return Err(io::Error::other(format!(
            "{operation}: metadata exceeds 1 MiB"
        )));
    }
    let mut bytes = Vec::new();
    stdout.take(1 << 20).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn failure(
    stderr: &mut File,
    status: ExitStatus,
    operation: &str,
) -> io::Result<io::Error> {
    stderr.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    stderr.take(8192).read_to_end(&mut bytes)?;
    Ok(io::Error::other(format!(
        "{operation} failed ({status}): {}",
        String::from_utf8_lossy(&bytes)
    )))
}

pub(crate) fn run_to_file(command: &mut Command, operation: &str) -> io::Result<File> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let status = command
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .status()
        .map_err(|error| io::Error::new(error.kind(), format!("{operation}: {error}")))?;
    if !status.success() {
        return Err(failure(&mut stderr, status, operation)?);
    }
    stdout.seek(SeekFrom::Start(0))?;
    Ok(stdout)
}

fn executable(name: &str) -> io::Result<PathBuf> {
    let filename = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let path = directory.join(&filename);
            if path.is_file() {
                return path.canonicalize();
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("media preparation requires {filename} on PATH; install FFmpeg and FFprobe"),
    ))
}

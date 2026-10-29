//! Offline Sony Cg compiler invocation shared by builtin and authored shaders.
//! The compiler is a host tool; exported players only consume GXP bytes.

use std::{
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

/// Flags are part of the effect code cache identity. Keep symbols for reflection.
pub const FLAGS: &[&str] = &[
    "-entry",
    "main",
    "-O3",
    "-nofastmath",
    "-nofastint",
    "-nocolor",
];

/// Native GXM shader stage and its Sony compiler profile.
#[derive(Clone, Copy, Debug)]
pub enum Stage {
    Vertex,
    Fragment,
}
impl Stage {
    fn profile(self) -> &'static str {
        match self {
            Self::Vertex => "sce_vp_psp2",
            Self::Fragment => "sce_fp_psp2",
        }
    }
}

/// Located, executable SDK compiler. `REEL_PSP2CGC` overrides SDK/PATH discovery.
pub struct Compiler {
    executable: PathBuf,
}
impl Compiler {
    /// Locate the official compiler using an explicit path, SDK root, or PATH.
    pub fn new() -> io::Result<Self> {
        let executable = if let Some(path) = env::var_os("REEL_PSP2CGC") {
            PathBuf::from(path)
        } else if let Some(root) = env::var_os("SCE_PSP2_SDK_DIR") {
            PathBuf::from(root).join("host_tools/bin/psp2cgc.exe")
        } else {
            let names: &[&str] = if cfg!(windows) {
                &["psp2cgc.exe"]
            } else {
                &["psp2cgc", "psp2cgc.exe"]
            };
            env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
                .find(|path| path.is_file())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "Sony psp2cgc compiler not found; set REEL_PSP2CGC or SCE_PSP2_SDK_DIR",
                    )
                })?
        };
        Ok(Self {
            executable: executable.canonicalize()?,
        })
    }

    /// Tool inputs watched by Cargo and hashed by the authored effect cache.
    pub fn tool_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths = vec![self.executable.clone()];
        for entry in fs::read_dir(self.executable.parent().unwrap())? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Feed versioned flags, filenames and complete tool contents to a hash.
    /// Absolute SDK installation paths never enter the content identity.
    pub fn fingerprint(&self, mut field: impl FnMut(&[u8])) -> io::Result<()> {
        field(b"Reel Sony GXP compiler v2\0sce_vp_psp2\0sce_fp_psp2\0fragment-D7203-retry-O2-no-spill");
        for flag in FLAGS {
            field(flag.as_bytes());
        }
        for path in self.tool_files()? {
            field(path.file_name().unwrap().to_string_lossy().as_bytes());
            let mut file = fs::File::open(path)?;
            field(&file.metadata()?.len().to_le_bytes());
            let mut buffer = [0; 65536];
            loop {
                let length = file.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                field(&buffer[..length]);
            }
        }
        Ok(())
    }

    /// Compile with exact math at O3. Fragment register exhaustion (D7203) retries
    /// the same source at O2 once, keeping spills as errors and both failure logs.
    pub fn compile(&self, stage: Stage, source: &str) -> io::Result<Vec<u8>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = env::temp_dir().join(format!(
            "reel-gxp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory)?;
        let temporary = Temporary(directory);
        let input = temporary.0.join("shader.cg");
        let output = temporary.0.join("shader.gxp");
        fs::write(&input, source)?;
        let input_path = self.host_path(&input)?;
        let output_path = self.host_path(&output)?;
        let run = |optimization| {
            Command::new(&self.executable)
                .args(["-profile", stage.profile()])
                .args(
                    FLAGS
                        .iter()
                        .copied()
                        .map(|flag| if flag == "-O3" { optimization } else { flag }),
                )
                .arg(&input_path)
                .arg("-o")
                .arg(&output_path)
                .output()
        };
        let mut compiled = run("-O3")?;
        let mut original = String::new();
        if !compiled.status.success()
            && matches!(stage, Stage::Fragment)
            && register_pressure(&compiled.stdout, &compiled.stderr)
        {
            original = format!(
                "O3 ({}):\n{}{}\nO2 retry:\n",
                compiled.status,
                String::from_utf8_lossy(&compiled.stdout),
                String::from_utf8_lossy(&compiled.stderr)
            );
            compiled = run("-O2").map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("{original}could not run compiler: {error}"),
                )
            })?;
        }
        if !compiled.status.success() {
            return Err(io::Error::other(format!(
                "Sony {} compilation failed ({}):\n{original}{}{}",
                stage.profile(),
                compiled.status,
                String::from_utf8_lossy(&compiled.stdout),
                String::from_utf8_lossy(&compiled.stderr)
            )));
        }
        let bytes = fs::read(output)?;
        if bytes.len() < 4 || &bytes[..4] != b"GXP\0" {
            return Err(io::Error::other(
                "Sony compiler produced an invalid GXP program",
            ));
        }
        Ok(bytes)
    }

    fn host_path(&self, path: &Path) -> io::Result<PathBuf> {
        if !cfg!(windows)
            && self
                .executable
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            let translated = Command::new("wslpath").arg("-w").arg(path).output()?;
            if !translated.status.success() {
                return Err(io::Error::other(
                    "wslpath could not translate the Sony compiler input",
                ));
            }
            Ok(PathBuf::from(
                String::from_utf8_lossy(&translated.stdout).trim(),
            ))
        } else {
            Ok(path.to_owned())
        }
    }
}

fn register_pressure(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].into_iter().any(|bytes| {
        String::from_utf8_lossy(bytes)
            .lines()
            .any(|line| line.trim_start().starts_with("error D7203:"))
    })
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::register_pressure;

    #[test]
    fn only_register_exhaustion_errors_admit_an_optimization_retry() {
        let exhausted = b"error D7203: not enough registers available for the entire program\r\n";
        assert!(register_pressure(exhausted, b""));
        assert!(register_pressure(b"", exhausted));
        assert!(!register_pressure(b"warning D7203: spilling enabled", b""));
        assert!(!register_pressure(b"error D1001: invalid syntax", b""));
        assert!(!register_pressure(b"source: error D7203: text", b""));
    }
}

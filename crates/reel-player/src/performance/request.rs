use super::capture::FrameCaptureOptions;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// One developer measurement through the ordinary package player.
/// Relative paths follow the player's working directory, as ordinary package paths do.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Request format; the supported version is 1.
    pub version: u32,
    /// ASB distribution to run, including its ordinary resource references.
    pub package: PathBuf,
    /// Persistent-data directory used by that package.
    pub saves: PathBuf,
    /// Directory receiving frames.tsv, optional frozen.tsv and status.json.
    pub output: PathBuf,
    /// Enable sampled interpreter internals; defaults to false.
    #[serde(default)]
    pub vm_timing: bool,
    /// Optional diagnostic override; omission preserves the renderer's policy.
    #[serde(default)]
    pub source_atlas: Option<bool>,
    /// Optional accepted-frame render-only measurement.
    #[serde(default)]
    pub frame: Option<FrameCaptureOptions>,
}

/// Terminal publication state for a developer request.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatusState {
    /// The player owns this request and its measurement is in progress.
    Running,
    /// Playback or the requested fixed-frame measurement completed and cleaned up.
    Complete,
    /// Playback, capture, capability validation or report publication failed.
    Error,
}

/// Measurement capabilities of the current player adapter.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct Capabilities {
    /// Accepted-frame replay through the production rendering adapter.
    pub fixed_frame: bool,
    /// Diagnostic serial completion drains for individual rendering passes.
    pub serial_pass: bool,
    /// Native GPU execution timestamps, separate from CPU submission wall time.
    pub gpu_timestamp: bool,
}
impl Capabilities {
    /// Capabilities supported by the adapter compiled for this process.
    pub const CURRENT: Self = Self {
        fixed_frame: cfg!(target_os = "vita"),
        serial_pass: cfg!(target_os = "vita"),
        gpu_timestamp: false,
    };
}

impl Request {
    /// Read and validate JSON at its configuration boundary.
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let request: Self =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Err(error) = request.validate() {
            let _ = request.write_status(StatusState::Error, Some(&error));
            return Err(error);
        }
        Ok(request)
    }
    /// Validate the schema and bounded capture settings without starting playback.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "unsupported performance request version {}",
                self.version
            ));
        }
        for (name, path) in [
            ("package", &self.package),
            ("saves", &self.saves),
            ("output", &self.output),
        ] {
            if path.as_os_str().is_empty() {
                return Err(format!("performance {name} path is empty"));
            }
        }
        if let Some(frame) = &self.frame {
            frame.validate().map_err(str::to_owned)?;
        }
        Ok(())
    }
    /// Publish state and adapter capabilities outside measured frame intervals.
    /// Callers publish completion only after ordinary session cleanup.
    pub fn write_status(&self, state: StatusState, error: Option<&str>) -> io::Result<()> {
        #[derive(Serialize)]
        struct Status<'a> {
            schema: u32,
            state: StatusState,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a str>,
            capabilities: Capabilities,
            request: &'a Request,
        }
        std::fs::create_dir_all(&self.output)?;
        let bytes = serde_json::to_vec_pretty(&Status {
            schema: 1,
            state,
            error,
            capabilities: Capabilities::CURRENT,
            request: self,
        })
        .map_err(io::Error::other)?;
        // Reports are ordinary files consumed by host tools and vdb. Vita's
        // persistent-save generations are resolved only by reel-storage readers.
        // Flush the next snapshot before replacing the previous one. During
        // Vita's no-replace rename, a polling reader can briefly see no status.
        use std::io::Write;
        let temporary = self.output.join("status.tmp");
        let target = self.output.join("status.json");
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        drop(file);
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::rename(temporary, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_defaults_preserve_render_policy_and_validate_capture_bounds() {
        let text = r#"{"version":1,"package":"game.asb","saves":"saves","output":"results"}"#;
        let request: Request = serde_json::from_str(text).unwrap();
        request.validate().unwrap();
        assert!(!request.vm_timing);
        assert_eq!(request.source_atlas, None);
        assert!(request.frame.is_none());
        for bad in [
            text.replace("\"version\":1", "\"version\":2"),
            text.replace("\"output\":\"results\"", "\"output\":\"\""),
        ] {
            assert!(
                serde_json::from_str::<Request>(&bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let frame = r#"{"label":"scene","accepted_frame":1,"repetitions":2,"cache":"replay"}"#;
        let frame: FrameCaptureOptions = serde_json::from_str(frame).unwrap();
        assert!(!frame.measure_passes);
        assert!(frame.validate().is_ok());
        assert!(
            serde_json::from_str::<FrameCaptureOptions>(
                r#"{"label":"scene","accepted_frame":1,"repetitions":2}"#
            )
            .is_err()
        );
    }
}

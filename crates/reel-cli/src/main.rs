use aside_compiler::SourceMap;
use codespan_reporting::{files::SimpleFile, term};
use reel_cli::{build, check, diagnostic_json, package_limits_json, read_source};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

const HELP: &str = "reel check <source.aside> [--format human|json]\nreel build <source.aside> --output <file.asb> [--entry main::name] [--overwrite] [--format human|json]\nreel dev <source.aside> --font <font.ttf> [--entry main::main] [--assets <directory>]\nreel run <file.asb> --font <font.ttf> [--assets <directory>]\n\nReel nodes and story services are available without imports in check, build and dev.\nDev opens a story from source; run opens a built story without recompiling it.\nDev and build default to main::main; --entry selects another story function. Existing output requires --overwrite.\nWindow commands require: cargo build -p reel-cli --features window\n";

struct Args {
    command: String,
    source: PathBuf,
    json: bool,
    output: Option<PathBuf>,
    entry: Option<String>,
    overwrite: bool,
    font: Option<PathBuf>,
    assets: Option<PathBuf>,
}
fn args(raw: &[OsString]) -> Result<Args, String> {
    if raw.first().is_none_or(|v| {
        !["check", "build", "dev", "run"]
            .iter()
            .any(|name| v == name)
    }) {
        return Err("expected check, build, dev or run".into());
    }
    let (mut source, mut format, mut output, mut entry) = (None, None, None, None);
    let (mut options, mut overwrite) = (true, false);
    let mut font = None;
    let mut assets = None;
    let mut iter = raw[1..].iter();
    while let Some(arg) = iter.next() {
        if options && arg == "--" {
            options = false;
        } else if options && arg == "--format" {
            if format.is_some() {
                return Err("--format may only be specified once".into());
            }
            format = Some(match iter.next().and_then(|v| v.to_str()) {
                Some("human") => false,
                Some("json") => true,
                _ => return Err("--format requires human or json".into()),
            });
        } else if options && arg == "--output" {
            if output.is_some() {
                return Err("--output may only be specified once".into());
            }
            output = Some(PathBuf::from(
                iter.next().ok_or("--output requires a file path")?,
            ));
        } else if options && arg == "--entry" {
            if entry.is_some() {
                return Err("--entry may only be specified once".into());
            }
            entry = Some(
                iter.next()
                    .and_then(|v| v.to_str())
                    .ok_or("--entry requires a UTF-8 main::name")?
                    .to_owned(),
            );
        } else if options && arg == "--font" {
            font = Some(PathBuf::from(
                iter.next().ok_or("--font requires a font file")?,
            ));
        } else if options && arg == "--assets" {
            assets = Some(PathBuf::from(
                iter.next().ok_or("--assets requires a directory")?,
            ));
        } else if options && arg == "--overwrite" {
            if overwrite {
                return Err("--overwrite may only be specified once".into());
            }
            overwrite = true;
        } else if options && arg.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", arg.to_string_lossy()));
        } else if source.replace(PathBuf::from(arg)).is_some() {
            return Err("expected exactly one source file".into());
        }
    }
    if raw[0] == "check" && (output.is_some() || entry.is_some() || overwrite) {
        return Err("--output, --entry and --overwrite are build-only options".into());
    }
    if raw[0] == "build" && output.is_none() {
        return Err("build requires an explicit --output <file.asb>".into());
    }
    let playing = raw[0] == "dev" || raw[0] == "run";
    if playing && (output.is_some() || overwrite || format.is_some()) {
        return Err("dev/run accept --font/--assets and dev also accepts --entry".into());
    }
    if playing && font.is_none() {
        return Err("dev/run require --font <font.ttf>".into());
    }
    if raw[0] == "run" && entry.is_some() {
        return Err("run uses the entry selected when the package was built".into());
    }
    if !playing && font.is_some() {
        return Err("--font is used by dev/run".into());
    }
    if !playing && assets.is_some() {
        return Err("--assets is used by dev/run".into());
    }
    if let Some(path) = &output
        && (path.to_str().is_none() || path.extension().is_none_or(|ext| ext != "asb"))
    {
        return Err("--output requires a UTF-8 .asb file path".into());
    }
    Ok(Args {
        command: raw[0].to_string_lossy().into_owned(),
        source: source.ok_or("missing source file")?,
        json: format.unwrap_or(false),
        output,
        entry,
        overwrite,
        font,
        assets,
    })
}
fn requested_json(raw: &[OsString]) -> bool {
    raw.iter()
        .take_while(|arg| *arg != "--")
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair[0] == "--format" && pair[1] == "json")
}
fn report_json(value: &Value) -> io::Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    writeln!(output)
}
fn failure(json_format: bool, stage: &str, path: Option<&str>, message: &str) -> io::Result<()> {
    if json_format {
        report_json(
            &json!({"schema_version": 1, "status": "error", "stage": stage,
            "source": path, "executed": false, "limits": package_limits_json(), "diagnostics": [{
                "code": format!("REEL-{}", stage.to_ascii_uppercase()), "severity": "error", "message": message, "range": null}]}),
        )
    } else {
        writeln!(
            io::stderr().lock(),
            "error[{stage}] {}: {message}",
            path.unwrap_or("reel")
        )
    }
}
fn run() -> io::Result<u8> {
    let raw: Vec<_> = std::env::args_os().skip(1).collect();
    let help = |arg: &OsString| arg == "--help" || arg == "-h";
    if (raw.len() == 1 && help(&raw[0]))
        || (raw.len() == 2
            && ["check", "build", "dev", "run"]
                .iter()
                .any(|name| raw[0] == *name)
            && help(&raw[1]))
    {
        write!(io::stdout().lock(), "{HELP}")?;
        return Ok(0);
    }
    if raw.len() == 1 && raw[0] == "--version" {
        writeln!(io::stdout().lock(), "reel {}", env!("CARGO_PKG_VERSION"))?;
        return Ok(0);
    }
    let parsed = match args(&raw) {
        Ok(args) => args,
        Err(message) => {
            failure(requested_json(&raw), "usage", None, &message)?;
            return Ok(2);
        }
    };
    let Some(path) = parsed.source.to_str() else {
        failure(
            parsed.json,
            "input",
            None,
            "source path must be UTF-8 for diagnostic reporting",
        )?;
        return Ok(2);
    };
    if parsed.command == "run" {
        let bytes = match std::fs::read(&parsed.source) {
            Ok(bytes) => bytes,
            Err(error) => {
                failure(false, "input", Some(path), &error.to_string())?;
                return Ok(2);
            }
        };
        return match reel_cli::decode_story(&bytes) {
            Ok(story) => launch(story, parsed.font.unwrap(), parsed.assets, path),
            Err(error) => {
                failure(false, "package", Some(path), &error.to_string())?;
                Ok(1)
            }
        };
    }
    let source = match read_source(&parsed.source) {
        Ok(source) => source,
        Err(error) => {
            if parsed.json {
                report_json(
                    &json!({"schema_version": 1, "status": "error", "source": path, "limits": package_limits_json(),
                    "stage": "input", "executed": false, "diagnostics": [{"code": error.code, "severity": "error", "message": error.message,
                        "range": error.byte_range.as_ref().map(|range| json!({"byte_start": range.start, "byte_end": range.end, "start": null, "end": null}))}]}),
                )?;
            } else {
                failure(false, "input", Some(path), &error.message)?;
            }
            return Ok(error.exit_code);
        }
    };
    let checked = match check(&source, path) {
        Ok(checked) => checked,
        Err(failed) => return source_error(parsed.json, path, &source, failed),
    };
    if parsed.command == "dev" {
        return match reel_cli::into_story(checked, parsed.entry.as_deref().unwrap_or("main::main"))
        {
            Ok(story) => launch(story, parsed.font.unwrap(), parsed.assets, path),
            Err(failed) => source_error(false, path, &source, failed),
        };
    }
    let result = (|| {
        let artifact = if parsed.output.is_some() {
            Some(build(
                &checked,
                Some(parsed.entry.as_deref().unwrap_or("main::main")),
            )?)
        } else {
            None
        };
        Ok((checked, artifact))
    })();
    match result {
        Ok((checked, artifact)) => {
            let mut cleanup_warning = None;
            if let (Some(output), Some(artifact)) = (&parsed.output, &artifact) {
                match reel_cli::output::write(
                    output,
                    &parsed.source,
                    &artifact.bytes,
                    parsed.overwrite,
                ) {
                    Ok(published) => cleanup_warning = published.cleanup_warning,
                    Err(error) => {
                        failure(parsed.json, "output", output.to_str(), &error.to_string())?;
                        return Ok(2);
                    }
                }
            }
            if parsed.json {
                report_json(
                    &json!({"schema_version": 1, "status": if artifact.is_some() { "built" } else { "verified" }, "source": path,
                    "scope": if artifact.is_some() { "static-compilation-asb-b01-roundtrip" } else { "static-compilation-b01-roundtrip" },
                    "executed": false, "runtime_admitted": false, "release_package": false, "diagnostics": [], "encoded_bytes": checked.bytes.len(),
                    "entry": {"kind": "module-initializer", "function_id": checked.compilation.entry.0},
                    "module": checked.compilation.metadata.canonical_path, "source_version": checked.compilation.metadata.source_version,
                    "limits": package_limits_json(),
                    "artifact": artifact.as_ref().map(|a| json!({"path": parsed.output.as_ref().and_then(|p| p.to_str()),
                        "format": "ASB", "major": 1, "minor": 0, "bytes": a.bytes.len(), "blake3": a.blake3,
                        "game_entry": a.game_entry, "module_only": a.game_entry.is_none(), "source_included": true,
                        "atomic_visibility": true, "power_loss_durability": "not_promised", "cleanup_warning": cleanup_warning}))}),
                )?;
            } else {
                writeln!(io::stdout().lock(), "{path}: check passed")?;
                if let Some(artifact) = &artifact {
                    writeln!(
                        io::stdout().lock(),
                        "Built {} ({} bytes). Ready to run.",
                        parsed.output.as_ref().unwrap().display(),
                        artifact.bytes.len(),
                    )?;
                    if let Some(warning) = cleanup_warning {
                        writeln!(io::stderr().lock(), "warning: {warning}")?;
                    }
                }
            }
            Ok(0)
        }
        Err(failed) => source_error(parsed.json, path, &source, failed),
    }
}

fn source_error(
    json_format: bool,
    path: &str,
    source: &str,
    failed: reel_cli::Failure,
) -> io::Result<u8> {
    if json_format {
        let map = SourceMap::new(source);
        let diagnostics: Vec<_> = failed
            .diagnostics
            .iter()
            .map(|d| {
                let mut value = diagnostic_json(d, &map);
                if let Some(code) = &failed.code {
                    value["code"] = json!(code);
                }
                value
            })
            .collect();
        report_json(
            &json!({"schema_version": 1, "status": "error", "source": path, "limits": package_limits_json(),
                    "stage": failed.stage, "executed": false, "diagnostics": diagnostics}),
        )?;
    } else {
        let file = SimpleFile::new(path, &source);
        let mut output = io::stderr().lock();
        for d in &failed.diagnostics {
            let mut rendered = d.to_codespan(());
            if let Some(code) = &failed.code {
                rendered.code = Some(code.clone());
            }
            term::emit_to_io_write(&mut output, &term::Config::default(), &file, &rendered)
                .map_err(io::Error::other)?;
        }
    }
    Ok(1)
}

fn launch(
    story: reel_engine::aside::source::CompiledStory,
    font: PathBuf,
    assets: Option<PathBuf>,
    path: &str,
) -> io::Result<u8> {
    #[cfg(feature = "window")]
    let result = {
        let mut options = reel_player::shell::StoryWindowOptions::new(font);
        options.images.root =
            assets.unwrap_or_else(|| std::path::Path::new(path).parent().unwrap().to_path_buf());
        reel_player::shell::run_story(story, options).map_err(|error| error.to_string())
    };
    #[cfg(not(feature = "window"))]
    let result: Result<(), String> = {
        let _ = (story, font, assets);
        Err("build the window player with: cargo build -p reel-cli --features window".into())
    };
    match result {
        Ok(()) => Ok(0),
        Err(message) => {
            failure(false, "player", Some(path), &message)?;
            Ok(1)
        }
    }
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "reel: cannot write diagnostic report: {error}"
            );
            ExitCode::from(2)
        }
    }
}

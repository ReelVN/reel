use aside_compiler::SourceMap;
use codespan_reporting::{files::SimpleFile, term};
use reel_cli::{check, diagnostic_json, read_source};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

const HELP: &str = "reel check <source.aside> [--format human|json]\n\nStatically parses, analyzes, lowers, encodes and verifies one bounded UTF-8 module.\nDoes not execute Aside, admit a runtime, resolve imports or register native APIs.\nNo release .asb/.rpk/release.json or game.toml support.\nInput limit: 8388608 bytes. Other compiler and B01 limits use their defaults.\nJSON: one report on stdout; ranges are half-open UTF-8 bytes, positions zero-based.\nExit codes: 0 verified, 1 source/compilation failure, 2 usage/IO failure.\n";

struct Args {
    source: PathBuf,
    json: bool,
}

fn args(raw: &[OsString]) -> Result<Args, String> {
    if raw.first().is_none_or(|v| v != "check") {
        return Err("expected `check`; dev/run/test/loc and release builds are unavailable".into());
    }
    let mut source = None;
    let mut format = None;
    let mut options = true;
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
        } else if options && arg.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option: {}", arg.to_string_lossy()));
        } else if source.replace(PathBuf::from(arg)).is_some() {
            return Err("expected exactly one source file".into());
        }
    }
    Ok(Args {
        source: source.ok_or("missing source file")?,
        json: format.unwrap_or(false),
    })
}

/// Error reporting still respects the option terminator, including invalid argv.
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
            "source": path, "executed": false, "diagnostics": [{"code": format!("REEL-{}", stage.to_ascii_uppercase()),
                "severity": "error", "message": message, "range": null}]}),
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
    if (raw.len() == 1 && (raw[0] == "--help" || raw[0] == "-h"))
        || (raw.len() == 2 && raw[0] == "check" && (raw[1] == "--help" || raw[1] == "-h"))
    {
        write!(io::stdout().lock(), "{HELP}")?;
        return Ok(0);
    }
    if raw.len() == 1 && raw[0] == "--version" {
        writeln!(
            io::stdout().lock(),
            "reel {} (static compiler; design 1.1)",
            env!("CARGO_PKG_VERSION")
        )?;
        return Ok(0);
    }
    let parsed = match args(&raw) {
        Ok(args) => args,
        Err(message) => {
            let json_format = requested_json(&raw);
            failure(json_format, "usage", None, &message)?;
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
    let source = match read_source(&parsed.source) {
        Ok(source) => source,
        Err(error) => {
            if parsed.json {
                report_json(
                    &json!({"schema_version": 1, "status": "error", "source": path,
                    "stage": "input", "executed": false, "diagnostics": [{
                        "code": error.code, "severity": "error", "message": error.message,
                        "range": error.byte_range.as_ref().map(|range| json!({
                            "byte_start": range.start, "byte_end": range.end, "start": null, "end": null
                        }))
                    }]}),
                )?;
            } else {
                failure(false, "input", Some(path), &error.message)?;
            }
            return Ok(error.exit_code);
        }
    };
    match check(&source) {
        Ok(checked) => {
            if parsed.json {
                report_json(
                    &json!({"schema_version": 1, "status": "verified", "source": path,
                    "scope": "static-compilation-b01-roundtrip", "executed": false, "runtime_admitted": false,
                    "release_package": false, "diagnostics": [], "encoded_bytes": checked.bytes.len(),
                    "entry": {"kind": "module-initializer", "function_id": checked.compilation.entry.0},
                    "module": checked.compilation.metadata.canonical_path,
                    "source_version": checked.compilation.metadata.source_version}),
                )?;
            } else {
                writeln!(
                    io::stdout().lock(),
                    "{path}: static compilation and B01 round trip verified ({} bytes; module initializer {}). No execution or runtime admission; no release package.",
                    checked.bytes.len(),
                    checked.compilation.entry.0
                )?;
            }
            Ok(0)
        }
        Err(failed) => {
            if parsed.json {
                let map = SourceMap::new(&source);
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
                    &json!({"schema_version": 1, "status": "error", "source": path,
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

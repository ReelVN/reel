mod commands;
use aside_compiler::{SourceMap, semantic::SourceModule};
use clap::Parser;
use codespan_reporting::{files::SimpleFile, term};
use commands::{Cli, Command, Localization, SourceCommand};
use reel_cli::{BuildProfile, build_with_profile, diagnostic_json, package_limits_json};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};
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
    let raw: Vec<_> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&raw) {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() && commands::requested_json(&raw[1..]) {
                failure(true, "usage", None, &error.to_string())?;
            } else {
                error.print()?;
            }
            return Ok(error.exit_code() as u8);
        }
    };
    match cli.command {
        Command::Source(command) => source_command(command),
        Command::Run(args) => run_package(args),
        Command::Loc(command) => localization_command(command),
        Command::PrepareImage(args) => prepare_image_command(args),
    }
}

fn run_package(args: commands::Run) -> io::Result<u8> {
    let Some(path) = args.source.to_str() else {
        failure(
            false,
            "input",
            None,
            "source path must be UTF-8 for diagnostic reporting",
        )?;
        return Ok(2);
    };
    match reel_cli::playback::run(
        &args.source,
        args.playback.assets,
        args.playback.save_directory,
    ) {
        Ok(()) => Ok(0),
        Err(error) => {
            failure(false, "package", Some(path), &error)?;
            Ok(1)
        }
    }
}

fn source_command(command: SourceCommand) -> io::Result<u8> {
    let json_format = command.format().json();
    let source = command.project().source.clone();
    let Some(path) = source.to_str() else {
        failure(
            json_format,
            "input",
            None,
            "source path must be UTF-8 for diagnostic reporting",
        )?;
        return Ok(2);
    };
    let mut modules = match command.project().module_roots() {
        Ok(modules) => modules,
        Err(error) => {
            failure(json_format, "usage", None, &error)?;
            return Ok(2);
        }
    };
    let mut project_path = source.clone();
    let mut requested_entry = command.entry().map(str::to_owned);
    let mut asset_root = None;
    if let SourceCommand::Dev(args) = &command {
        asset_root = args.playback.assets.clone();
        if source
            .extension()
            .is_some_and(|extension| extension == "asb")
        {
            let manifest = match reel_cli::distribution::read_manifest(&source) {
                Ok(manifest) => manifest,
                Err(error) => {
                    failure(false, "project", Some(path), &error.to_string())?;
                    return Ok(1);
                }
            };
            let Some(project) = manifest.source else {
                failure(
                    false,
                    "project",
                    Some(path),
                    "release export has no source project",
                )?;
                return Ok(1);
            };
            let root = source.parent().unwrap();
            project_path = root.join(project.entry);
            for (alias, directory) in project.modules {
                modules.entry(alias).or_insert_with(|| root.join(directory));
            }
            requested_entry = requested_entry.or(Some(project.function));
            asset_root =
                asset_root.or_else(|| manifest.loose.map(|directory| root.join(directory)));
        }
    }
    let loaded = match reel_cli::project::load_with_modules(&project_path, &modules) {
        Ok(project) => project,
        Err(error) if error.sources.is_empty() => {
            if let Some(input) = error.input_error {
                if json_format {
                    report_json(
                        &json!({"schema_version": 1, "status": "error", "source": path, "stage": "input", "executed": false,
                        "diagnostics": [{"code": input.code, "severity": "error", "message": input.message,
                        "range": input.byte_range.map(|range| json!({"byte_start": range.start, "byte_end": range.end, "start": null, "end": null}))}]}),
                    )?;
                } else {
                    failure(false, "input", Some(path), &input.message)?;
                }
                return Ok(input.exit_code);
            }
            failure(
                json_format,
                error.failure.stage,
                Some(path),
                &error.failure.diagnostics[0].diagnostic.message,
            )?;
            return Ok(2);
        }
        Err(error) => return source_error(json_format, path, &error.sources, error.failure),
    };
    let inputs = &loaded.sources;
    let entry = loaded.entry(requested_entry.as_deref());
    let checked = match loaded.check() {
        Ok(checked) => checked,
        Err(error) => return source_error(json_format, path, inputs, error),
    };
    match command {
        SourceCommand::Check(_) => compilation_report(&checked, None, None, json_format, path),
        SourceCommand::Build(args) => {
            let artifact = match build_with_profile(&checked, Some(&entry), args.settings.profile())
            {
                Ok(artifact) => artifact,
                Err(error) => return source_error(json_format, path, inputs, error),
            };
            let published = match reel_cli::output::write(
                &args.output,
                &loaded.input,
                &artifact.bytes,
                args.settings.overwrite,
            ) {
                Ok(published) => published,
                Err(error) => {
                    failure(
                        json_format,
                        "output",
                        args.output.to_str(),
                        &error.to_string(),
                    )?;
                    return Ok(2);
                }
            };
            compilation_report(
                &checked,
                Some((&artifact, &args.output)),
                published.cleanup_warning.as_deref(),
                json_format,
                path,
            )
        }
        SourceCommand::Export(args) => {
            let profile = args.settings.profile();
            let player = if profile == BuildProfile::Release {
                let player = reel_cli::distribution::player_path(&std::env::current_exe()?);
                if !player.is_file() {
                    failure(
                        json_format,
                        "export",
                        player.to_str(),
                        "platform player is missing; build with: cargo build --release -p reel-cli --features window",
                    )?;
                    return Ok(1);
                }
                Some(player)
            } else {
                None
            };
            let artifact = match reel_cli::build_export(&checked, &entry, profile) {
                Ok(artifact) => artifact,
                Err(error) => return source_error(json_format, path, inputs, error),
            };
            let options = reel_cli::distribution::ExportOptions {
                output: args.output,
                overwrite: args.settings.overwrite,
                resources: args.resources,
                text: args.text,
                modules,
                entry,
                pack_bytes: args
                    .pack_bytes
                    .unwrap_or(std::num::NonZeroU64::new(2 << 30).unwrap()),
                player,
            };
            match reel_cli::distribution::export(&loaded, &artifact, &options) {
                Ok(result) => {
                    if json_format {
                        report_json(
                            &json!({"status":"exported", "profile":profile.name(), "output":options.output,
                            "resources":result.files,"payload_bytes":result.bytes,
                            "text_pool_bytes":artifact.text_pool.as_ref().map_or(0, |pool| pool.as_bytes().len())}),
                        )?;
                    } else {
                        writeln!(
                            io::stdout().lock(),
                            "Exported {} ({}; {} resources, {} payload bytes)",
                            options.output.display(),
                            profile.name(),
                            result.files,
                            result.bytes
                        )?;
                    }
                    Ok(0)
                }
                Err(error) => {
                    failure(
                        json_format,
                        "export",
                        options.output.to_str(),
                        &error.to_string(),
                    )?;
                    Ok(1)
                }
            }
        }
        SourceCommand::Dev(args) => match reel_cli::into_story(checked, &entry) {
            Ok(story) => launch(
                story,
                asset_root.or(Some(loaded.root)),
                None,
                args.playback.save_directory,
                path,
            ),
            Err(error) => source_error(false, path, inputs, error),
        },
    }
}

fn compilation_report(
    checked: &reel_cli::Checked,
    built: Option<(&reel_cli::Artifact, &Path)>,
    cleanup_warning: Option<&str>,
    json_format: bool,
    path: &str,
) -> io::Result<u8> {
    if json_format {
        report_json(
            &json!({"schema_version": 1, "status": if built.is_some() { "built" } else { "verified" }, "source": path,
            "scope": if built.is_some() { "static-compilation-asb-b01-roundtrip" } else { "static-compilation-b01-roundtrip" },
            "executed": false, "runtime_admitted": false, "release_package": false, "diagnostics": [],
            "encoded_bytes": built.map_or(checked.bytes.len(), |(a, _)| a.bytecode_bytes),
            "entry": {"kind": "project-initializer", "function_id": checked.compilation.initializer.0},
            "modules": checked.modules.iter().map(|m| json!({"module": m.canonical_path, "source_version": m.source_version})).collect::<Vec<_>>(),
            "limits": package_limits_json(),
            "artifact": built.map(|(a, output)| json!({"path": output.to_str(), "format": "ASB", "major": reel_package::VERSION, "minor": 0,
                "bytes": a.bytes.len(), "blake3": a.blake3, "game_entry": a.game_entry, "module_only": a.game_entry.is_none(),
                "profile": a.profile.name(), "source_included": a.profile == BuildProfile::Debug,
                "atomic_visibility": true, "power_loss_durability": "not_promised", "cleanup_warning": cleanup_warning}))}),
        )?;
    } else {
        writeln!(io::stdout().lock(), "{path}: check passed")?;
        if let Some((artifact, output)) = built {
            writeln!(
                io::stdout().lock(),
                "Built {} ({} bytes, {}). Ready to run.",
                output.display(),
                artifact.bytes.len(),
                artifact.profile.name()
            )?;
            if let Some(warning) = cleanup_warning {
                writeln!(io::stderr().lock(), "warning: {warning}")?;
            }
        }
    }
    Ok(0)
}

fn localization_command(command: Localization) -> io::Result<u8> {
    match command {
        Localization::Check(args) => {
            match reel_cli::localization::check(&args.source, &args.locale) {
                Ok(()) => {
                    writeln!(
                        io::stdout().lock(),
                        "Catalog checked: {}",
                        args.source.display()
                    )?;
                    Ok(0)
                }
                Err(error) => {
                    failure(false, "localization", args.source.to_str(), &error)?;
                    Ok(1)
                }
            }
        }
        Localization::Bundle(args) => {
            match reel_cli::localization::bundle(&args.source, &args.output, args.overwrite) {
                Ok(result) => {
                    writeln!(
                        io::stdout().lock(),
                        "Bundled {} entries from {} catalogs: {}",
                        result.entries,
                        result.files,
                        args.output.display()
                    )?;
                    if let Some(warning) = result.cleanup_warning {
                        writeln!(io::stderr().lock(), "warning: {warning}")?;
                    }
                    Ok(0)
                }
                Err(error) => {
                    failure(false, "localization", args.source.to_str(), &error)?;
                    Ok(1)
                }
            }
        }
        Localization::Extract(args) => {
            let (output, extraction) = if let Some(output) = args.destination.output {
                let result = reel_cli::localization::extract(&args.source, &output, args.overwrite);
                (output, result)
            } else {
                let output = args
                    .destination
                    .output_dir
                    .expect("clap requires one extraction output");
                let result = reel_cli::localization::extract_directory(
                    &args.source,
                    &output,
                    args.overwrite,
                );
                (output, result)
            };
            match extraction {
                Ok(result) => {
                    writeln!(
                        io::stdout().lock(),
                        "Extracted {} messages from {} calls; skipped {} dynamic calls: {}",
                        result.messages,
                        result.occurrences,
                        result.skipped.len(),
                        output.display()
                    )?;
                    for skipped in result.skipped {
                        writeln!(io::stderr().lock(), "warning[localization]: {skipped}")?;
                    }
                    for stale in result.stale_catalogs {
                        writeln!(
                            io::stderr().lock(),
                            "warning[localization]: {}: no current source messages; file left in place, review before bundling",
                            stale.display()
                        )?;
                    }
                    if let Some(warning) = result.cleanup_warning {
                        writeln!(io::stderr().lock(), "warning: {warning}")?;
                    }
                    Ok(0)
                }
                Err(error) => {
                    failure(false, "localization", args.source.to_str(), &error)?;
                    Ok(1)
                }
            }
        }
    }
}

fn prepare_image_command(args: commands::PrepareImage) -> io::Result<u8> {
    match reel_cli::prepare_image::prepare(&args.source, &args.output, args.tile_size) {
        Ok(manifest) => {
            writeln!(
                io::stdout().lock(),
                "Prepared {}×{}, {} levels, tile interior {}: {}",
                manifest.width,
                manifest.height,
                manifest.levels(),
                manifest.tile_edge,
                args.output.display()
            )?;
            Ok(0)
        }
        Err(error) => {
            failure(false, "prepare-image", args.source.to_str(), &error)?;
            Ok(1)
        }
    }
}
fn source_error(
    json_format: bool,
    path: &str,
    sources: &[SourceModule],
    failed: reel_cli::Failure,
) -> io::Result<u8> {
    if json_format {
        let diagnostics: Vec<_> = failed
            .diagnostics
            .iter()
            .map(|d| {
                let input = sources
                    .iter()
                    .find(|input| d.source.as_deref() == input.source_path.as_deref())
                    .unwrap_or(&sources[0]);
                let mut value = diagnostic_json(&d.diagnostic, &SourceMap::new(&input.source));
                value["source"] = json!(input.source_path.as_deref().unwrap_or(path));
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
        let mut output = io::stderr().lock();
        for d in &failed.diagnostics {
            let input = sources
                .iter()
                .find(|input| d.source.as_deref() == input.source_path.as_deref())
                .unwrap_or(&sources[0]);
            let file = SimpleFile::new(input.source_path.as_deref().unwrap_or(path), &input.source);
            let mut rendered = d.diagnostic.to_codespan(());
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
    assets: Option<PathBuf>,
    packages: Option<(Vec<PathBuf>, Option<PathBuf>)>,
    save_directory: Option<PathBuf>,
    path: &str,
) -> io::Result<u8> {
    let root = assets.unwrap_or_else(|| Path::new(path).parent().unwrap().to_path_buf());
    let result = reel_cli::playback::launch(story, root, packages, save_directory);
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

mod cleaning;
mod commands;
mod formatting;
mod progress;
use aside_compiler::{SourceMap, semantic::SourceModule};
use clap::Parser;
use codespan_reporting::{files::SimpleFile, term};
use commands::{Cli, Command, SourceCommand};
use reel_cli::{BuildProfile, diagnostic_json, package_limits_json, prepare_build};
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
        Command::PrepareImage(args) => prepare_image_command(args),
        Command::PrepareTexture(args) => prepare_texture_command(args),
        Command::PrepareResources(args) => prepare_resources_command(args),
        Command::Clean(args) => cleaning::run(args.resources),
        Command::Fmt(args) => formatting::run(args.check),
    }
}

fn run_package(args: commands::Run) -> io::Result<u8> {
    let source = if let Some(source) = args.source {
        source
    } else {
        match reel_cli::project::locate(Path::new(".")) {
            Ok(project) => project.root.join(if args.release {
                "build/release/game.asb"
            } else {
                "build/debug/game.asb"
            }),
            Err(error) => {
                failure(false, "project", None, &error.to_string())?;
                return Ok(2);
            }
        }
    };
    let Some(path) = source.to_str() else {
        failure(
            false,
            "input",
            None,
            "source path must be UTF-8 for diagnostic reporting",
        )?;
        return Ok(2);
    };
    match reel_cli::playback::run(
        &source,
        args.playback.assets,
        args.playback.save_directory,
        args.playback.resource_target,
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
    let mut progress = progress::Progress::new(json_format);
    let path = "src/main.aside";
    let cache = if let SourceCommand::Dev(args) = &command {
        progress.phase("Checking development cache");
        let cache = reel_cli::development::Cache::probe(Path::new("."), command.target()).ok();
        if let Some(cache) = &cache
            && let Some(story) = cache.read()
        {
            progress.phase("Preparing resources");
            let (assets, resource_files) = match reel_cli::playback::prepare_story_assets(
                cache.project_root(),
                args.playback.assets.clone(),
                &args.playback.resource_target,
                &story.effects,
            ) {
                Ok(resources) => resources,
                Err(error) => {
                    progress.stop();
                    failure(false, "resources", None, &error.to_string())?;
                    return Ok(1);
                }
            };
            progress.finish("Using cached code; starting game");
            return launch(
                story,
                assets,
                args.playback.save_directory.clone(),
                args.playback.resource_target.clone(),
                resource_files,
                path,
            );
        }
        cache
    } else {
        None
    };
    progress.phase("Reading project");
    let mut report = |event: aside_compiler::Progress| {
        use aside_compiler::ProgressStage;
        let label = match event.stage {
            ProgressStage::Discover => "Reading project",
            ProgressStage::Parse => "Parsing modules",
            ProgressStage::Analyze => "Analyzing modules",
            ProgressStage::Lower => "Compiling modules",
            ProgressStage::Verify => "Preparing executable code",
        };
        progress.update(label, event.completed, event.total);
    };
    let loaded = match reel_cli::project::load_with_progress(Path::new("."), &mut report) {
        Ok(project) => project,
        Err(error) if error.sources.is_empty() => {
            progress.stop();
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
        Err(error) => {
            progress.stop();
            return source_error(json_format, path, &error.sources, error.failure);
        }
    };
    let inputs = &loaded.sources;
    let entry = reel_cli::project::ENTRY;
    progress.phase("Compiling source");
    let checked = match loaded.check_with_progress(command.target(), &mut report) {
        Ok(checked) => checked,
        Err(error) => {
            progress.stop();
            return source_error(json_format, path, inputs, error);
        }
    };
    match command {
        SourceCommand::Check(_) => {
            progress.phase("Checking translations");
            if let Err(error) =
                reel_cli::localization::check_project(&loaded.project_root, &checked.catalogs)
            {
                progress.stop();
                failure(json_format, "localization", Some(path), &error.to_string())?;
                return Ok(1);
            }
            progress.finish("Checked");
            compilation_report(&checked, None, None, json_format, path)
        }
        SourceCommand::Build(args) => {
            progress.phase("Building ASB");
            let code = match prepare_build(&checked, Some(entry), args.settings.profile()) {
                Ok(artifact) => artifact,
                Err(error) => {
                    progress.stop();
                    return source_error(json_format, path, inputs, error);
                }
            };
            let default_output = args.output.is_none();
            let output = args.output.unwrap_or_else(|| {
                loaded
                    .project_root
                    .join(format!("build/{}/game.asb", args.settings.profile().name()))
            });
            if default_output && let Err(error) = std::fs::create_dir_all(output.parent().unwrap())
            {
                progress.stop();
                failure(json_format, "output", output.to_str(), &error.to_string())?;
                return Ok(2);
            }
            let archive_directory = output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let archive_directory = match archive_directory.canonicalize() {
                Ok(directory) if directory.is_dir() => directory,
                Ok(_) => {
                    progress.stop();
                    failure(
                        json_format,
                        "output",
                        output.to_str(),
                        "output parent must be a directory",
                    )?;
                    return Ok(2);
                }
                Err(error) => {
                    progress.stop();
                    failure(json_format, "output", output.to_str(), &error.to_string())?;
                    return Ok(2);
                }
            };
            progress.phase("Packing translations");
            let prepared = match reel_cli::localization::prepare_project(
                &loaded.project_root,
                &checked.catalogs,
                &archive_directory,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    progress.stop();
                    failure(json_format, "localization", Some(path), &error.to_string())?;
                    return Ok(1);
                }
            };
            progress.phase("Preparing effect shaders");
            let manifest = reel_cli::prepare_effects::prepare(
                &loaded.project_root,
                &checked.effects,
                reel_cli::prepare_effects::Target::for_export(&args.resource_target),
            )
            .and_then(|effects| reel_cli::prepare_effects::archive(&effects, &archive_directory))
            .and_then(|effects| {
                reel_cli::distribution::build_manifest(&prepared, effects.as_deref())
            });
            let manifest = match manifest {
                Ok(manifest) => manifest,
                Err(error) => {
                    progress.stop();
                    failure(json_format, "effects", Some(path), &error.to_string())?;
                    return Ok(1);
                }
            };
            let artifact = match code.finish(manifest.as_ref()) {
                Ok(artifact) => artifact,
                Err(error) => {
                    progress.stop();
                    return source_error(json_format, path, inputs, error);
                }
            };
            progress.phase("Writing build");
            let published = match reel_cli::output::write(
                &output,
                &loaded.input,
                &artifact.bytes,
                default_output || args.settings.overwrite,
            ) {
                Ok(published) => published,
                Err(error) => {
                    progress.stop();
                    failure(json_format, "output", output.to_str(), &error.to_string())?;
                    return Ok(2);
                }
            };
            progress.finish("Built");
            compilation_report(
                &checked,
                Some((&artifact, &output)),
                published.cleanup_warning.as_deref(),
                json_format,
                path,
            )
        }
        SourceCommand::Export(args) => {
            progress.phase("Preparing export");
            let profile = args.settings.profile();
            let runtime = if let Some(directory) = args.runtime {
                Some(reel_cli::distribution::Runtime::Directory(directory))
            } else if profile == BuildProfile::Release && !args.data_only {
                let player = reel_cli::distribution::player_path(&std::env::current_exe()?);
                if !player.is_file() {
                    progress.stop();
                    failure(
                        json_format,
                        "export",
                        player.to_str(),
                        "platform player is missing; download the complete Reel tool package, including reel-game",
                    )?;
                    return Ok(1);
                }
                Some(reel_cli::distribution::Runtime::Executable(player))
            } else {
                None
            };
            progress.phase("Building ASB");
            let artifact = match reel_cli::build_export(&checked, entry, profile) {
                Ok(artifact) => artifact,
                Err(error) => {
                    progress.stop();
                    return source_error(json_format, path, inputs, error);
                }
            };
            let default_output = args.output.is_none();
            let resources = if args.resources.is_empty()
                && args.text.is_empty()
                && loaded.project_root.join("resources").is_dir()
            {
                vec![reel_cli::distribution::Mount {
                    prefix: ".".into(),
                    directory: loaded.project_root.join("resources"),
                }]
            } else {
                args.resources
            };
            let options = reel_cli::distribution::ExportOptions {
                output: args.output.unwrap_or_else(|| {
                    loaded
                        .project_root
                        .join("dist")
                        .join(&args.resource_target)
                        .join(profile.name())
                }),
                overwrite: default_output || args.settings.overwrite,
                resources,
                text: args.text,
                resource_target: args.resource_target,
                pack_bytes: args
                    .pack_bytes
                    .unwrap_or(std::num::NonZeroU64::new(2 << 30).unwrap()),
                runtime,
            };
            progress.phase("Packing resources");
            match reel_cli::distribution::export(&loaded, &artifact, &options) {
                Ok(result) => {
                    progress.finish("Exported");
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
                    progress.stop();
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
        SourceCommand::Dev(args) => {
            progress.phase("Saving development cache");
            if let Some(cache) = cache
                && let Err(error) = cache.store(&checked)
            {
                progress.stop();
                writeln!(io::stderr().lock(), "warning: compilation cache: {error}")?;
            }
            progress.phase("Preparing resources");
            let (assets, resource_files) = match reel_cli::playback::prepare_story_assets(
                &loaded.project_root,
                args.playback.assets,
                &args.playback.resource_target,
                &checked.effects,
            ) {
                Ok(resources) => resources,
                Err(error) => {
                    progress.stop();
                    failure(false, "resources", None, &error.to_string())?;
                    return Ok(1);
                }
            };
            match reel_cli::into_story(checked, entry) {
                Ok(story) => {
                    drop(loaded);
                    progress.finish("Starting game");
                    launch(
                        story,
                        assets,
                        args.playback.save_directory,
                        args.playback.resource_target,
                        resource_files,
                        path,
                    )
                }
                Err(error) => {
                    progress.stop();
                    source_error(false, path, inputs, error)
                }
            }
        }
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
            "scope": if built.is_some() { "static-compilation-asb" } else { "static-compilation" },
            "executed": false, "runtime_admitted": false, "release_package": false, "diagnostics": [],
            "encoded_bytes": built.map_or(checked.compilation.program.stats().encoded_bytes, |(a, _)| a.bytecode_bytes),
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

fn prepare_image_command(args: commands::PrepareImage) -> io::Result<u8> {
    match reel_cli::prepare_image::prepare(
        &args.source,
        &args.output,
        args.tile_size,
        args.tile_encoding,
    ) {
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

fn prepare_resources_command(args: commands::PrepareResources) -> io::Result<u8> {
    let json_format = args.format.json();
    let mut progress = progress::Progress::new(json_format);
    progress.phase("Preparing resources");
    let result = reel_cli::project::locate(Path::new(".")).and_then(|project| {
        let target = reel_package::resource_target::ResourceTarget::parse(&args.resource_target)?;
        reel_cli::resource_conversion::prepare_project_with_limit(
            &project.root,
            &target,
            args.memory_bytes.get(),
        )
    });
    match result {
        Ok(prepared) => {
            progress.finish("Prepared resources");
            if json_format {
                report_json(
                    &json!({"schema_version":1,"status":"prepared","resource_target":args.resource_target,
                    "resources":prepared.files.len(),"index":prepared.index,"report":prepared.report}),
                )?;
            } else {
                writeln!(
                    io::stdout().lock(),
                    "Prepared {} resources for {} ({} converted, {} reused, {} unchanged; {} payload bytes)",
                    prepared.files.len(),
                    args.resource_target,
                    prepared.report.converted,
                    prepared.report.reused,
                    prepared.report.unchanged,
                    prepared.report.output_bytes
                )?;
            }
            Ok(0)
        }
        Err(error) => {
            progress.stop();
            failure(
                json_format,
                "resources",
                Some("resources.toml"),
                &error.to_string(),
            )?;
            Ok(1)
        }
    }
}
fn prepare_texture_command(args: commands::PrepareTexture) -> io::Result<u8> {
    let options = reel_cli::prepare_texture::Options {
        max_size: args.size,
        gxm_layout: false,
        power_of_two: args.power_of_two,
        mipmaps: args.mipmaps,
        format: args.format,
        dither: args.dither,
    };
    match reel_cli::prepare_texture::prepare(&args.source, &args.output, options) {
        Ok(header) => {
            let descriptor = header.descriptor();
            writeln!(
                io::stdout().lock(),
                "Prepared {:?}: logical {:?}, content {:?}, storage {:?}, {} texture bytes, {} payload bytes: {}",
                descriptor.format,
                descriptor.layout.logical_size,
                descriptor.layout.content_size,
                descriptor.layout.storage_size,
                descriptor.byte_len().unwrap(),
                header.payload_len(),
                args.output.display()
            )?;
            Ok(0)
        }
        Err(error) => {
            failure(false, "prepare-texture", args.source.to_str(), &error)?;
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
    assets: PathBuf,
    save_directory: Option<PathBuf>,
    resource_target: String,
    resource_files: Option<std::collections::BTreeMap<String, PathBuf>>,
    path: &str,
) -> io::Result<u8> {
    let mut options = reel_player::player::PlayerOptions::default();
    options.assets.root = assets;
    options.save_directory = save_directory;
    options.resource_target = resource_target;
    options.resource_files = resource_files;
    let result = reel_cli::playback::launch(story, options);
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

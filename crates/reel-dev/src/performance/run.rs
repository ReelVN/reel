use super::*;
use std::time::Instant;

pub(super) fn execute(options: Run) -> Result<()> {
    let directory = fs::canonicalize(&options.directory)?;
    ensure!(
        !directory.join("reports/status.json").exists(),
        "this run already has a result; prepare a new run directory"
    );
    if let Some(policy) = &options.policy {
        return verify::evaluate(&directory, policy, |manifest| {
            play(&options, &directory, manifest)
        });
    }
    let manifest = Manifest::read(&directory)?;
    play(&options, &directory, &manifest)?;
    collect::analyze(&directory, &manifest).map(|_| ())
}

fn play(options: &Run, directory: &Path, manifest: &Manifest) -> Result<()> {
    if manifest.environment == "emulator" {
        emulator::run(options, directory, manifest)
    } else if manifest.target == "vita" {
        device(options, directory, manifest)
    } else {
        ensure!(
            options.address.is_none() && options.emulator.is_none(),
            "a desktop run uses its local player"
        );
        let executable = directory.join("game").join(if cfg!(windows) {
            "reel-game.exe"
        } else {
            "reel-game"
        });
        checked(
            Command::new(executable)
                .current_dir(directory.join("game"))
                .arg("--save-directory")
                .arg(&manifest.request.saves)
                .env("REEL_PERFORMANCE_CONFIG", directory.join("request.json")),
            &directory.join("player.log"),
            Duration::from_secs(options.timeout_seconds),
        )?;
        Ok(())
    }
}

fn device(options: &Run, directory: &Path, manifest: &Manifest) -> Result<()> {
    let address = options
        .address
        .as_deref()
        .context("Vita run requires --address")?;
    let vdb = Vdb {
        path: &options.vdb,
        address,
    };
    vdb.call(&["status".as_ref()])?;
    let status_path = manifest.request.output.join("status.json");
    ensure!(
        vdb.status(&status_path)?.is_none(),
        "device already has this run; use perf collect or prepare a new run name"
    );
    if let Some(runtime) = &options.runtime {
        ensure!(
            runtime.join("eboot.bin").is_file(),
            "runtime requires eboot.bin"
        );
        // Installing the performance VPK registers the application.
        let app = format!("ux0:/app/{VITA_PLAYER_TITLE_ID}");
        let eboot = format!("{app}/eboot.bin");
        vdb.call(&["stat".as_ref(), eboot.as_ref()])
            .context("install the VPK from reel-dev perf runtime before deploying a runtime")?;
        vdb.push(&runtime.join("eboot.bin"), Path::new(&eboot))?;
        for name in ["sce_sys", "licenses", "module"] {
            let path = runtime.join(name);
            let remote = format!("{app}/{name}");
            if path.is_dir() {
                vdb.call(&[
                    "push-dir".as_ref(),
                    path.as_os_str(),
                    Path::new(&remote).as_os_str(),
                ])?;
            } else if path.is_file() {
                vdb.push(&path, Path::new(&remote))?;
            }
        }
    }
    let game = manifest
        .request
        .package
        .parent()
        .context("game directory")?;
    println!("Deploying {}", game.display());
    vdb.call(&[
        "push-dir".as_ref(),
        directory.join("game").as_os_str(),
        game.as_os_str(),
    ])?;
    vdb.push(
        &directory.join("request.json"),
        Path::new("ux0:/data/reel/performance.json"),
    )?;
    let stdout = fs::File::create(directory.join("transport.log"))?;
    let stderr = stdout.try_clone()?;
    let _logging = LogChild(
        vdb.command()
            .arg("logs")
            .arg("-o")
            .arg(directory.join("device.log"))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()?,
    );
    vdb.call(&["launch".as_ref(), VITA_PLAYER_TITLE_ID.as_ref()])?;
    let started = Instant::now();
    let mut next_progress = Duration::from_secs(20);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if let Some(status) = vdb.status(&status_path)?
            && status["state"] != "running"
        {
            break;
        }
        if started.elapsed() >= next_progress {
            println!(
                "Waiting for the player: {} seconds",
                started.elapsed().as_secs()
            );
            next_progress += Duration::from_secs(20);
        }
        if started.elapsed() >= Duration::from_secs(options.timeout_seconds) {
            let _ = vdb.call(&["quit".as_ref(), VITA_PLAYER_TITLE_ID.as_ref()]);
            bail!("player timed out; reports can be retrieved with perf collect");
        }
    }
    collect::download(&vdb, manifest, directory)
}

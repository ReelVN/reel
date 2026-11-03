use super::*;
use std::time::Instant;

pub(super) fn execute(options: Run) -> Result<()> {
    let directory = fs::canonicalize(&options.directory)?;
    let manifest = Manifest::read(&directory)?;
    ensure!(
        !directory.join("reports/status.json").exists(),
        "this run already has a result; prepare a new run directory"
    );
    if manifest.environment == "emulator" {
        emulator::run(&options, &directory, &manifest)?;
    } else if manifest.target == "vita" {
        device(&options, &directory, &manifest)?;
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
        let result = checked(
            Command::new(executable)
                .current_dir(directory.join("game"))
                .arg("--save-directory")
                .arg(&manifest.request.saves)
                .env("REEL_PERFORMANCE_CONFIG", directory.join("request.json")),
            &directory.join("player.log"),
            Duration::from_secs(options.timeout_seconds),
        );
        result?;
    }
    collect::report(&directory)
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
        // The official player must already be registered by installing its VPK.
        vdb.call(&["stat".as_ref(), "ux0:/app/REELP0001/eboot.bin".as_ref()])
            .context("install the official reel-vita VPK once before deploying a runtime")?;
        vdb.push(
            &runtime.join("eboot.bin"),
            Path::new("ux0:/app/REELP0001/eboot.bin"),
        )?;
        for name in ["launcher.rpk", "sce_sys", "licenses", "module"] {
            let path = runtime.join(name);
            if path.is_dir() {
                vdb.call(&[
                    "push-dir".as_ref(),
                    path.as_os_str(),
                    Path::new(&format!("ux0:/app/REELP0001/{name}")).as_os_str(),
                ])?;
            } else if path.is_file() {
                vdb.push(&path, Path::new(&format!("ux0:/app/REELP0001/{name}")))?;
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
    vdb.call(&["launch".as_ref(), "REELP0001".as_ref()])?;
    let started = Instant::now();
    let mut next_progress = Duration::from_secs(20);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if let Some(status) = vdb.status(&status_path)? {
            if status["state"] != "running" {
                break;
            }
        }
        if started.elapsed() >= next_progress {
            println!(
                "Waiting for the player: {} seconds",
                started.elapsed().as_secs()
            );
            next_progress += Duration::from_secs(20);
        }
        if started.elapsed() >= Duration::from_secs(options.timeout_seconds) {
            let _ = vdb.call(&["quit".as_ref(), "REELP0001".as_ref()]);
            bail!("player timed out; reports can be retrieved with perf collect");
        }
    }
    collect::download(&vdb, manifest, directory)
}

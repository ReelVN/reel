use super::*;

pub(super) fn device(options: &Device) -> Result<()> {
    let manifest = Manifest::read(&options.directory)?;
    ensure!(manifest.target == "vita", "collect requires a Vita run");
    if manifest.environment == "emulator" {
        let storage = options
            .storage
            .as_ref()
            .context("emulator collection requires --storage")?;
        emulator::collect(storage, &options.directory, &manifest)?;
        return report(&options.directory);
    }
    let vdb = Vdb {
        path: &options.vdb,
        address: options
            .address
            .as_deref()
            .context("Vita collection requires --address")?,
    };
    download(&vdb, &manifest, &options.directory)?;
    report(&options.directory)
}

pub(super) fn download(vdb: &Vdb<'_>, manifest: &Manifest, directory: &Path) -> Result<()> {
    let reports = directory.join("reports");
    fs::create_dir_all(&reports)?;
    vdb.pull(
        &manifest.request.output.join("status.json"),
        &reports.join("status.json"),
    )?;
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(reports.join("status.json"))?)?;
    validate_status(&status, manifest)?;
    ensure!(status["state"] != "running", "measurement is still running");
    if status["state"] == "complete" {
        vdb.pull(
            &manifest.request.output.join("frames.tsv"),
            &reports.join("frames.tsv"),
        )?;
        if manifest.request.frame.is_some() {
            vdb.pull(
                &manifest.request.output.join("frozen.tsv"),
                &reports.join("frozen.tsv"),
            )?;
        }
        if manifest
            .request
            .frame
            .as_ref()
            .is_some_and(|f| f.save_frame)
        {
            vdb.pull(
                &manifest.request.output.join("frame.postcard"),
                &reports.join("frame.postcard"),
            )?;
        }
    }
    Ok(())
}

pub(super) fn report(directory: &Path) -> Result<()> {
    let manifest = Manifest::read(directory)?;
    let reports = directory.join("reports");
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(reports.join("status.json"))?)?;
    validate_status(&status, &manifest)?;
    ensure!(
        status["state"] == "complete",
        "performance session {}: {}",
        status["state"],
        status["error"]
    );
    let environment = &manifest.environment;
    crate::frame_cost::report(
        environment,
        &reports.join("frames.tsv"),
        &reports.join("frames.json"),
        None,
    )?;
    if manifest.request.frame.is_some() {
        crate::render_cost::report(
            environment,
            &reports.join("frozen.tsv"),
            &reports.join("render.json"),
        )?;
        let result: serde_json::Value =
            serde_json::from_slice(&fs::read(reports.join("render.json"))?)?;
        ensure!(
            result["valid"] == true,
            "fixed-frame trace is incomplete; see render.json"
        );
    }
    summary::report(&reports)?;
    println!("Performance reports: {}", reports.display());
    Ok(())
}

fn validate_status(status: &serde_json::Value, manifest: &Manifest) -> Result<()> {
    ensure!(status["schema"] == 1, "unsupported player status version");
    ensure!(
        status["request"] == serde_json::to_value(&manifest.request)?,
        "player status belongs to a different measurement request"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_results_must_match_the_requested_workload_and_measurement() {
        let request = Request {
            version: 1,
            package: "ux0:/game/game.asb".into(),
            saves: "ux0:/game/saves".into(),
            output: "ux0:/results/run".into(),
            vm_timing: false,
            source_atlas: None,
            frame: None,
        };
        let manifest = Manifest {
            schema: 1,
            target: "vita".into(),
            environment: "vita".into(),
            project: "project".into(),
            exporter: "reel".into(),
            request,
        };
        let mut status =
            serde_json::json!({"schema":1,"state":"complete","request":manifest.request});
        validate_status(&status, &manifest).unwrap();
        status["request"]["vm_timing"] = true.into();
        assert!(validate_status(&status, &manifest).is_err());
        status["request"] = serde_json::Value::Null;
        assert!(validate_status(&status, &manifest).is_err());
    }
}

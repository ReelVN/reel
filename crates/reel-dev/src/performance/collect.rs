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
    analyze(directory, &manifest).map(|_| ())
}

pub(super) fn analyze(directory: &Path, manifest: &Manifest) -> Result<serde_json::Value> {
    let status = read_status(directory, manifest)?;
    ensure!(
        status["state"] == "complete",
        "performance session {}: {}",
        status["state"],
        status["error"]
    );
    analyze_frames(directory, manifest)
}

pub(super) fn read_status(directory: &Path, manifest: &Manifest) -> Result<serde_json::Value> {
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("reports/status.json"))?)?;
    validate_status(&status, manifest)?;
    Ok(status)
}

pub(super) fn analyze_frames(directory: &Path, manifest: &Manifest) -> Result<serde_json::Value> {
    let reports = directory.join("reports");
    let environment = &manifest.environment;
    let input = reports.join("frames.tsv");
    let raw = fs::read_to_string(&input).context("read probe log")?;
    validate_frame_request(&raw, &manifest.request)?;
    let frames = crate::frame_cost::report_text(
        environment,
        &input,
        &raw,
        &reports.join("frames.json"),
        None,
    )?;
    let render = if manifest.request.frame.is_some() {
        let result = crate::render_cost::report(
            environment,
            &reports.join("frozen.tsv"),
            &reports.join("render.json"),
        )?;
        ensure!(
            result["valid"] == true,
            "fixed-frame trace is incomplete; see render.json"
        );
        Some(result)
    } else {
        None
    };
    let summary = summary::report(&reports, &frames, render.as_ref(), &raw)?;
    println!("Performance reports: {}", reports.display());
    Ok(summary)
}

fn validate_status(status: &serde_json::Value, manifest: &Manifest) -> Result<()> {
    ensure!(status["schema"] == 1, "unsupported player status version");
    ensure!(
        status["request"] == serde_json::to_value(&manifest.request)?,
        "player status belongs to a different measurement request"
    );
    Ok(())
}

fn validate_frame_request(raw: &str, request: &Request) -> Result<()> {
    let header = raw
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("performance_request\t"))
        .context("frames.tsv is missing its measurement request")?;
    let recorded: serde_json::Value =
        serde_json::from_str(header).context("read frames.tsv measurement request")?;
    ensure!(
        recorded == serde_json::to_value(request)?,
        "frames.tsv belongs to a different measurement request"
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

    #[test]
    fn frame_report_must_belong_to_its_measurement_request() {
        let request = Request {
            version: 1,
            package: "game.asb".into(),
            saves: "saves".into(),
            output: "reports".into(),
            vm_timing: false,
            source_atlas: None,
            frame: None,
        };
        let header = |request: &Request| {
            format!(
                "performance_request\t{}\r\nperformance_scope\tordinary_session\r\n",
                serde_json::to_string(request).unwrap()
            )
        };
        validate_frame_request(&header(&request), &request).unwrap();
        let mut other = request.clone();
        other.output = "other/reports".into();
        assert!(validate_frame_request(&header(&other), &request).is_err());
        assert!(validate_frame_request("performance_scope\tordinary_session\n", &request).is_err());
    }
}

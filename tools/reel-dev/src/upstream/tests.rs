use super::*;
use std::io::Read;
use std::sync::Barrier;

static NEXT: AtomicUsize = AtomicUsize::new(0);
fn temporary() -> PathBuf {
    let dir = maintenance::artifacts().unwrap().join(format!(
        "observer-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    dir
}
fn outcome(code: i32, stdout: Vec<u8>, stderr: Vec<u8>, timed_out: bool) -> process::Outcome {
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    };
    process::Outcome {
        status,
        stdout,
        stderr,
        timed_out,
    }
}
fn sample_job(name: &str, kind: Kind) -> Job {
    Job {
        fixture: format!("testData/exec/{name}.nut"),
        golden: format!("testData/exec/{name}{}", kind.suffix()),
        kind,
        seeds: vec!["default"],
    }
}
fn runner<'a>(dir: &'a Path, backend: &'a Backend<'a>) -> Runner<'a> {
    Runner {
        program: Path::new("fake-sq-never-executed"),
        work: dir,
        out: dir,
        timeout: TEST_TIMEOUT,
        backend,
        comparison_lock: Mutex::new(()),
    }
}

#[test]
fn complete_invocation_plan_matches_the_recorded_1494_upstream_observations() {
    // Digest of sorted [fixture,kind,seed,golden,args] from the frozen T01
    // upstream.json; only the transient diagnostic output path is <ACTUAL>.
    let jobs = plan(&root().join("third_party/quirrel")).unwrap();
    assert_eq!(jobs.len(), 1296);
    let mut rows = Vec::new();
    for job in jobs {
        for seed in &job.seeds {
            rows.push((
                job.fixture.clone(),
                job.kind.name(),
                *seed,
                job.golden.clone(),
                arguments(&job, seed, Path::new("<ACTUAL>")).unwrap(),
            ));
        }
    }
    rows.sort();
    assert_eq!(rows.len(), 1494);
    assert_eq!(
        sha(&serde_json::to_vec(&rows).unwrap()),
        "d36de864f89b4ea9d90b7e100368d225dc3913c69ff8169cc7c0447577bc7da7"
    );
}

#[test]
fn exact_modes_first_dot_selection_and_seed_text_rules() {
    for (kind, flags, suffix, redirect) in [
        (Kind::Exec, vec!["--check-stack"], ".out", true),
        (Kind::Diagnostics, vec!["-diag-file"], ".diag.txt", false),
        (Kind::Ast, vec!["-ast-dump"], ".opt.txt", false),
        (
            Kind::Analyzer,
            vec!["-sa", "-diag-file"],
            ".diag.txt",
            false,
        ),
        (Kind::Types, vec!["--parse-types"], ".out", true),
    ] {
        let job = sample_job("file", kind);
        assert!(job.golden.ends_with(suffix));
        assert_eq!(kind.redirect(), redirect);
        let mut expected = vec!["-optCH", "-iter-seed:255"];
        expected.extend(flags);
        if !redirect {
            expected.push("OUT");
        }
        expected.push("testData/exec/file.nut");
        assert_eq!(arguments(&job, "255", Path::new("OUT")).unwrap(), expected);
    }
    for source in ["foreach x,y", "// .map", "obj.reduce", ".eachThing"] {
        assert_eq!(seeds(source), ["default", "0", "255"]);
    }
    for source in ["foreach x\r,y", "foreach x\n,y", ".keys()", "foreach value"] {
        assert_eq!(seeds(source), ["default"]);
    }
    let dir = temporary();
    for (path, _) in ROOTS {
        fs::create_dir_all(dir.join(path)).unwrap();
    }
    write_file(&dir.join("testData/exec/plain.nut.txt"), b"// .map").unwrap();
    write_file(
        &dir.join("testData/exec/skip.extra.nut"),
        b"ignored by first-dot suffix check",
    )
    .unwrap();
    let jobs = plan(&dir).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].fixture, "testData/exec/plain.nut.txt");
    assert_eq!(jobs[0].golden, "testData/exec/plain.out");
    assert_eq!(jobs[0].seeds, ["default", "0", "255"]);
}

#[test]
fn universal_newlines_python_rstrip_and_line_counts_are_exact() {
    for (actual, expected) in [
        (&b"one\r\ntwo\r"[..], &b"one\ntwo\n"[..]),
        (b"one\x1c\n", b"one\n"),
        (b"x \t\n", b"x\n"),
        (b"", b""),
    ] {
        assert!(compare(actual, expected).unwrap());
    }
    assert!(!compare(b"x\n\n", b"x\n").unwrap());
    assert!(!compare(b"\n", b"").unwrap());
    assert!(!compare("x\u{2028}y".as_bytes(), b"x\ny").unwrap());
    assert!(compare(b"\xff", b"x").is_err());
}

#[test]
fn each_seed_is_archived_before_reuse_and_missing_goldens_stay_unasserted() {
    let dir = temporary();
    let backend = |inv: Invocation<'_>| -> Result<process::Outcome> {
        let seed = inv
            .args
            .iter()
            .find_map(|a| a.strip_prefix("-iter-seed:"))
            .unwrap_or("default");
        let bytes = format!("{seed}\r\n").into_bytes();
        inv.redirect.unwrap().try_clone()?.write_all(&bytes)?;
        Ok(outcome(0, vec![], vec![], false))
    };
    let r = runner(&dir, &backend);
    let mut job = sample_job("seeds", Kind::Exec);
    job.seeds = vec!["default", "0", "255"];
    let mut records = r.execute(&[job]).unwrap();
    assert_eq!(records.len(), 3);
    let by_seed = |seed: &str| records.iter().find(|r| r.seed == seed).unwrap();
    assert_eq!(by_seed("default").status, "unasserted");
    assert_eq!(by_seed("0").status, "mismatch");
    assert_eq!(by_seed("255").status, "mismatch");
    for record in &records {
        let expected = format!("{}\r\n", record.seed).into_bytes();
        assert_eq!(
            fs::read(dir.join(record.actual.as_ref().unwrap())).unwrap(),
            expected
        );
        assert_eq!(record.actual_sha256.as_ref().unwrap(), &sha(&expected));
    }
    assert_eq!(
        fs::read(dir.join("testData/exec/seeds.out")).unwrap(),
        b"default\n"
    );
    archive(&dir, &mut records).unwrap();
    assert!(!dir.join("raw").exists());
    let mut zip =
        zip::ZipArchive::new(File::open(dir.join("upstream-artifacts.zip")).unwrap()).unwrap();
    for record in &records {
        let mut bytes = Vec::new();
        let mut entry = zip.by_name(record.actual.as_ref().unwrap()).unwrap();
        assert_eq!(entry.last_modified(), Some(DateTime::default()));
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(sha(&bytes), *record.actual_sha256.as_ref().unwrap());
        assert_eq!(
            fs::read(dir.join(record.readable_actual.as_ref().unwrap())).unwrap(),
            bytes
        );
    }
}

#[test]
fn session_seals_mock_evidence_and_preserves_runner_and_bridge_exits() {
    // These fixtures are synthetic text and never interpreted. This exercises
    // the same copy/version/worker/archive/drift/exit path as run-upstream.
    for scenario in [
        "pass",
        "mismatch",
        "crash",
        "timeout",
        "spawn-error",
        "missing-golden",
        "startup-error",
        "final-version-error",
        "snapshot-error",
    ] {
        let directory = temporary();
        let vendor = directory.join("vendor");
        for (path, _) in ROOTS {
            fs::create_dir_all(vendor.join(path)).unwrap();
        }
        write_file(
            &vendor.join("testData/exec/mock.nut"),
            b"// synthetic .map fixture; never executed\n",
        )
        .unwrap();
        if scenario != "missing-golden" {
            write_file(&vendor.join("testData/exec/mock.out"), b"ok\n").unwrap();
        }
        let pins: BTreeMap<_, _> = files(&vendor)
            .unwrap()
            .into_iter()
            .map(|path| {
                (
                    path.strip_prefix(&vendor)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    sha(&fs::read(&path).unwrap()),
                )
            })
            .collect();
        let manifest = json!({"commit":"synthetic-test-snapshot","files_sha256":pins});
        let program = directory.join("mock-compiler-not-executed");
        fs::write(&program, b"a hashable mock, never spawned").unwrap();
        let out = directory.join("out");
        let work = directory.join("work");
        let versions = AtomicUsize::new(0);
        let backend = |inv: Invocation<'_>| -> Result<process::Outcome> {
            if inv.args == ["-v"] {
                let query = versions.fetch_add(1, Ordering::Relaxed);
                if scenario == "startup-error" && query == 0 {
                    return Ok(outcome(
                        7,
                        b"version failed".to_vec(),
                        b"error".to_vec(),
                        false,
                    ));
                }
                if scenario == "final-version-error" && query == 1 {
                    return Ok(outcome(0, b"\xff".to_vec(), vec![], false));
                }
                return Ok(outcome(0, b"mock version\n".to_vec(), vec![], false));
            }
            assert_eq!(inv.timeout, TEST_TIMEOUT);
            assert_eq!(inv.args.last().unwrap(), "testData/exec/mock.nut");
            assert_eq!(
                fs::read(inv.cwd.join(inv.args.last().unwrap()))?,
                b"// synthetic .map fixture; never executed\n"
            );
            if scenario == "spawn-error" {
                anyhow::bail!("mock subprocess could not spawn");
            }
            inv.redirect
                .unwrap()
                .try_clone()?
                .write_all(if scenario == "mismatch" {
                    b"wrong\n"
                } else {
                    b"ok\n"
                })?;
            Ok(outcome(
                if scenario == "crash" { 7 } else { 0 },
                vec![],
                vec![],
                scenario == "timeout",
            ))
        };
        let checked = std::sync::atomic::AtomicBool::new(false);
        let result = Session {
            program: &program,
            vendor: &vendor,
            out: &out,
            work: &work,
            manifest: &manifest,
            timeout: TEST_TIMEOUT,
            backend: &backend,
        }
        .run(|| {
            checked.store(true, Ordering::Relaxed);
            if scenario == "snapshot-error" {
                anyhow::bail!("mock source pin failure");
            }
            Ok(())
        });
        assert!(checked.load(Ordering::Relaxed));
        assert_eq!(versions.load(Ordering::Relaxed), 2);
        let summary: Value =
            serde_json::from_slice(&fs::read(out.join("upstream.json")).unwrap()).unwrap();
        let expected_runner = match scenario {
            "spawn-error" => 2,
            "mismatch" | "crash" | "timeout" | "startup-error" => 1,
            _ => 0,
        };
        assert_eq!(summary["runner_exit_code"], expected_runner, "{scenario}");
        assert_eq!(summary["threads"], 12);
        assert_eq!(summary["timeout_seconds"], 10);
        assert_eq!(summary["sq_sha256"], sha(b"a hashable mock, never spawned"));
        assert_eq!(summary["changed_snapshot_files_in_work"], json!([]));
        assert_eq!(
            summary["created_goldens_in_work"],
            if scenario == "missing-golden" {
                json!(["testData/exec/mock.out"])
            } else {
                json!([])
            }
        );
        if scenario == "snapshot-error" {
            assert!(result.is_err());
            assert!(
                summary["snapshot_error"]
                    .as_str()
                    .unwrap()
                    .contains("mock source pin failure")
            );
        } else {
            assert_eq!(result.unwrap(), i32::from(scenario != "pass"), "{scenario}");
        }
        let runs = summary["runs"].as_array().unwrap();
        assert_eq!(
            runs.len(),
            match scenario {
                "startup-error" => 0,
                "spawn-error" => 1,
                _ => 3,
            }
        );
        if scenario == "missing-golden" {
            assert_eq!(
                runs.iter().find(|r| r["seed"] == "default").unwrap()["status"],
                "unasserted"
            );
            assert!(
                runs.iter()
                    .filter(|r| r["seed"] != "default")
                    .all(|r| r["status"] == "pass")
            );
        }
        let mut zip =
            zip::ZipArchive::new(File::open(out.join("upstream-artifacts.zip")).unwrap()).unwrap();
        for record in runs {
            let mut bytes = Vec::new();
            zip.by_name(record["actual"].as_str().unwrap())
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(record["actual_sha256"], sha(&bytes));
        }
        assert!(!out.join("raw").exists());
        assert!(out.join("upstream.log").is_file());
        assert!(out.join("versions/startup.stdout").is_file());
        assert!(out.join("versions/final.stdout").is_file());
        // All mutations are in work; the reference fixture/golden never change.
        for (name, digest) in &pins {
            assert_eq!(sha(&fs::read(vendor.join(name)).unwrap()), *digest);
        }
        assert_eq!(
            vendor.join("testData/exec/mock.out").exists(),
            scenario != "missing-golden"
        );
    }
}

#[test]
fn all_status_precedence_and_process_stream_observation_are_preserved() {
    for (code, timeout, actual, golden, status) in [
        (
            0,
            false,
            b"pass\n".as_slice(),
            Some(b"pass\n".as_slice()),
            "pass",
        ),
        (0, false, b"wrong", Some(b"pass".as_slice()), "mismatch"),
        (0, false, b"created", None, "unasserted"),
        (7, false, b"partial", Some(b"pass".as_slice()), "crash"),
        (9, true, b"partial", Some(b"pass".as_slice()), "timeout"),
        (
            0,
            false,
            b"\xff",
            Some(b"pass".as_slice()),
            "observer_error",
        ),
    ] {
        let dir = temporary();
        let job = sample_job("case", Kind::Diagnostics);
        if let Some(bytes) = golden {
            write_file(&dir.join(&job.golden), bytes).unwrap();
        }
        let backend = |inv: Invocation<'_>| -> Result<process::Outcome> {
            fs::write(&inv.args[inv.args.len() - 2], actual)?;
            Ok(outcome(
                code,
                b"stdout".to_vec(),
                b"stderr".to_vec(),
                timeout,
            ))
        };
        let record = runner(&dir, &backend).observe(&job, "default");
        assert_eq!(record.status, status);
        assert_eq!(record.returncode, Some(code));
        assert_eq!(
            fs::read(dir.join(record.process_stdout.as_ref().unwrap())).unwrap(),
            b"stdout"
        );
        assert_eq!(
            fs::read(dir.join(record.process_stderr.as_ref().unwrap())).unwrap(),
            b"stderr"
        );
        assert_eq!(
            fs::read(dir.join(record.actual.as_ref().unwrap())).unwrap(),
            actual
        );
        assert_eq!(
            runner_exit(&[record]),
            match status {
                "pass" | "unasserted" => 0,
                "observer_error" => 2,
                _ => 1,
            }
        );
    }
    let dir = temporary();
    let backend =
        |_: Invocation<'_>| -> Result<process::Outcome> { anyhow::bail!("simulated spawn error") };
    let mut job = sample_job("spawn", Kind::Exec);
    job.seeds = vec!["default", "0", "255"];
    let records = runner(&dir, &backend).execute(&[job]).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "observer_error");
    assert!(
        records[0]
            .observer_error
            .as_ref()
            .unwrap()
            .contains("simulated spawn error")
    );
    assert_eq!(runner_exit(&records), 2);
}

#[test]
fn twelve_workers_and_per_fixture_seed_order_are_retained() {
    let dir = temporary();
    let barrier = Barrier::new(WORKERS);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let observed = Mutex::new(BTreeMap::<String, Vec<String>>::new());
    let mut jobs = Vec::new();
    for i in 0..WORKERS {
        let mut job = sample_job(&format!("parallel-{i}"), Kind::Exec);
        job.seeds = vec!["default", "0", "255"];
        write_file(&dir.join(&job.golden), b"ok").unwrap();
        jobs.push(job);
    }
    let backend = |inv: Invocation<'_>| -> Result<process::Outcome> {
        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(current, Ordering::SeqCst);
        let seed = inv
            .args
            .iter()
            .find_map(|a| a.strip_prefix("-iter-seed:"))
            .unwrap_or("default");
        observed
            .lock()
            .unwrap()
            .entry(inv.args.last().unwrap().clone())
            .or_default()
            .push(seed.to_owned());
        if seed == "default" {
            barrier.wait();
        }
        inv.redirect.unwrap().try_clone()?.write_all(b"ok")?;
        active.fetch_sub(1, Ordering::SeqCst);
        Ok(outcome(0, vec![], vec![], false))
    };
    let records = runner(&dir, &backend).execute(&jobs).unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 12);
    assert_eq!(records.len(), 36);
    assert!(records.iter().all(|r| r.status == "pass"));
    for seeds in observed.into_inner().unwrap().values() {
        assert_eq!(seeds, &["default", "0", "255"]);
    }
}

#[test]
fn snapshot_drift_missing_goldens_and_artifact_path_guards_fail_closed() {
    let dir = temporary();
    fs::create_dir_all(dir.join("doc/examples")).unwrap();
    write_file(&dir.join("testData/exec/a.nut"), b"original").unwrap();
    let manifest = json!({"files_sha256":{"testData/exec/a.nut":sha(b"original")}});
    assert_eq!(drift(&dir, &manifest).unwrap(), (vec![], vec![]));
    write_file(&dir.join("testData/exec/a.nut"), b"changed").unwrap();
    write_file(&dir.join("testData/exec/a.out"), b"new").unwrap();
    let (changed, created) = drift(&dir, &manifest).unwrap();
    assert_eq!(changed, ["testData/exec/a.nut"]);
    assert_eq!(created, ["testData/exec/a.out"]);
    assert_eq!(bridge_exit(0, &changed, &created), 1);
    assert_eq!(bridge_exit(2, &changed, &created), 1);
    assert!(new_artifact_path(&dir, &dir).is_err());
    assert!(new_artifact_path(&dir.join("../outside"), &dir).is_err());
    assert!(new_artifact_path(&root().join("source-output"), &dir).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root(), dir.join("escape")).unwrap();
        assert!(new_artifact_path(&dir.join("escape/new"), &dir).is_err());
    }
}

#[cfg(unix)]
#[test]
fn bounded_process_handles_bytes_exit_codes_redirection_and_timeout_trees() {
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "printf '\\377out'; printf 'err' >&2; exit 7"]),
        Duration::from_secs(2),
        None,
    )
    .unwrap();
    assert!(!result.timed_out);
    assert_eq!(process::return_code(result.status), Some(7));
    assert_eq!(result.stdout, b"\xffout");
    assert_eq!(result.stderr, b"err");
    let dir = temporary();
    let redirect = File::create(dir.join("combined")).unwrap();
    let result = process::run(
        Command::new("/bin/sh").args(["-c", "printf 'out'; printf 'err' >&2"]),
        Duration::from_secs(2),
        Some(&redirect),
    )
    .unwrap();
    assert!(result.status.success());
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    assert_eq!(fs::read(dir.join("combined")).unwrap(), b"outerr");
    // The wrapper spawns a real descendant and exits. The descendant retains
    // both pipes; a wait-only timeout or killing only the wrapper would hang.
    let pidfile = dir.join("descendant.pid");
    let started = Instant::now();
    let result = process::run(
        Command::new("/bin/sh")
            .args([
                "-c",
                "sleep 30 & echo $! > \"$1\"; printf 'partial'; printf 'error' >&2; exit 0",
                "mock",
            ])
            .arg(&pidfile),
        Duration::from_millis(80),
        None,
    )
    .unwrap();
    assert!(result.timed_out);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(result.stdout, b"partial");
    assert_eq!(result.stderr, b"error");
    let pid = fs::read_to_string(pidfile).unwrap();
    if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
        assert_eq!(
            stat.split(") ").nth(1).unwrap().chars().next(),
            Some('Z'),
            "descendant still running"
        );
    }
    assert!(
        process::run(
            &mut Command::new("/nonexistent/observer-process"),
            Duration::from_millis(10),
            None
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn native_observer_uses_only_a_mock_executable_and_retains_timeout_raw_bytes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temporary();
    let executable = dir.join("mock-sq.sh");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf 'partial'; printf 'error' >&2\nsleep 30\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = Runner {
        program: &executable,
        work: &dir,
        out: &dir,
        timeout: Duration::from_millis(80),
        backend: &native,
        comparison_lock: Mutex::new(()),
    };
    let job = sample_job("timeout", Kind::Exec);
    write_file(&dir.join(&job.golden), b"expected").unwrap();
    let record = runner.observe(&job, "default");
    assert_eq!(record.status, "timeout");
    assert!(record.timed_out);
    assert_eq!(record.returncode, Some(-9));
    assert_eq!(
        fs::read(dir.join(record.actual.unwrap())).unwrap(),
        b"partialerror"
    );
    assert_eq!(record.actual_sha256.unwrap(), sha(b"partialerror"));
}

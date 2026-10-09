//! Rust port of the fixed Quirrel runner/observation contract, without Python hooks.
//! The imported runner remains unchanged as the provenance and behavior reference.
use crate::{files, fixtures, maintenance, process, root, sha};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

pub const WORKERS: usize = 12;
pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const ROOTS: [(&str, Kind); 6] = [
    ("testData/exec", Kind::Exec),
    ("testData/diagnostics", Kind::Diagnostics),
    ("testData/ast", Kind::Ast),
    ("testData/static_analyzer", Kind::Analyzer),
    ("testData/types", Kind::Types),
    ("doc/examples", Kind::Exec),
];

#[derive(Clone, Copy, Debug)]
enum Kind {
    Exec,
    Diagnostics,
    Ast,
    Analyzer,
    Types,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Exec => "Exec",
            Self::Diagnostics => "Diagnostics",
            Self::Ast => "AST",
            Self::Analyzer => "Static Analyzer",
            Self::Types => "Parse Types",
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::Diagnostics | Self::Analyzer => ".diag.txt",
            Self::Ast => ".opt.txt",
            _ => ".out",
        }
    }
    fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Exec => &["--check-stack"],
            Self::Diagnostics => &["-diag-file"],
            Self::Ast => &["-ast-dump"],
            Self::Analyzer => &["-sa", "-diag-file"],
            Self::Types => &["--parse-types"],
        }
    }
    fn redirect(self) -> bool {
        matches!(self, Self::Exec | Self::Types)
    }
}

#[derive(Clone, Debug)]
struct Job {
    fixture: String,
    golden: String,
    kind: Kind,
    seeds: Vec<&'static str>,
}

// Python TextIO universal newlines followed by its exact text seed heuristic.
fn text_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
fn seeds(source: &str) -> Vec<&'static str> {
    let source = text_newlines(source);
    if source.split('\n').any(|line| {
        line.find("foreach")
            .is_some_and(|i| line[i + 7..].contains(','))
    }) || [".map", ".reduce", ".each"]
        .iter()
        .any(|s| source.contains(s))
    {
        vec!["default", "0", "255"]
    } else {
        vec!["default"]
    }
}
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
fn compare(actual: &[u8], expected: &[u8]) -> Result<bool> {
    let actual = text_newlines(std::str::from_utf8(actual)?);
    let expected = text_newlines(std::str::from_utf8(expected)?);
    Ok(actual
        .split_inclusive('\n')
        .map(|s| s.trim_end_matches(python_space))
        .eq(expected
            .split_inclusive('\n')
            .map(|s| s.trim_end_matches(python_space))))
}

fn plan(vendor: &Path) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for (directory, kind) in ROOTS {
        for path in files(&vendor.join(directory))? {
            let basename = path
                .file_name()
                .unwrap()
                .to_str()
                .context("non-UTF8 fixture path")?;
            // runTestForData splits at the FIRST dot, not Path::extension().
            let (name, suffix) = basename
                .split_once('.')
                .context("upstream test filename has no dot")?;
            if !matches!(suffix, "nut" | "nut.txt") {
                continue;
            }
            let parent = path.parent().unwrap();
            let source = if parent.join(format!("{name}.nut")).exists() {
                parent.join(format!("{name}.nut"))
            } else {
                parent.join(format!("{name}.nut.txt"))
            };
            jobs.push(Job {
                fixture: source
                    .strip_prefix(vendor)?
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                golden: parent
                    .join(format!("{name}{}", kind.suffix()))
                    .strip_prefix(vendor)?
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                kind,
                seeds: seeds(&fs::read_to_string(source)?),
            });
        }
    }
    Ok(jobs)
}

fn arguments(job: &Job, seed: &str, actual: &Path) -> Result<Vec<String>> {
    let mut args = vec!["-optCH".to_owned()];
    if seed != "default" {
        args.push(format!("-iter-seed:{seed}"));
    }
    args.extend(job.kind.flags().iter().map(|s| (*s).to_owned()));
    if !job.kind.redirect() {
        args.push(actual.to_str().context("non-UTF8 output path")?.to_owned());
    }
    args.push(job.fixture.clone());
    Ok(args)
}

#[derive(Debug, Serialize)]
struct Record {
    fixture: String,
    kind: &'static str,
    seed: String,
    artifact_dir: String,
    golden: String,
    golden_present: bool,
    timed_out: bool,
    elapsed_seconds: f64,
    command: Vec<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    returncode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readable_actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison_matched: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observer_error: Option<String>,
}

struct Invocation<'a> {
    program: &'a Path,
    args: &'a [String],
    cwd: &'a Path,
    redirect: Option<&'a File>,
    timeout: Duration,
}
type Backend<'a> = dyn Fn(Invocation<'_>) -> Result<process::Outcome> + Sync + 'a;
fn native(invocation: Invocation<'_>) -> Result<process::Outcome> {
    Ok(process::run(
        Command::new(invocation.program)
            .args(invocation.args)
            .current_dir(invocation.cwd),
        invocation.timeout,
        invocation.redirect,
    )?)
}

struct Runner<'a> {
    program: &'a Path,
    work: &'a Path,
    out: &'a Path,
    timeout: Duration,
    backend: &'a Backend<'a>,
    // Match the upstream printLock boundary around compare/updateExpected.
    comparison_lock: Mutex<()>,
}
impl Runner<'_> {
    fn observe(&self, job: &Job, seed: &str) -> Record {
        let started = Instant::now();
        let expected = self.work.join(&job.golden);
        let actual = self.work.join("actual").join(&job.golden);
        let mut record = Record {
            fixture: job.fixture.clone(),
            kind: job.kind.name(),
            seed: seed.to_owned(),
            artifact_dir: format!("raw/{}/{seed}", job.fixture),
            golden: job.golden.clone(),
            golden_present: expected.exists(),
            timed_out: false,
            elapsed_seconds: 0.,
            command: vec![self.program.to_string_lossy().into_owned()],
            status: "observer_error",
            returncode: None,
            process_stdout: None,
            process_stderr: None,
            actual: None,
            actual_sha256: None,
            readable_actual: None,
            comparison_matched: None,
            observer_error: None,
        };
        let result = (|| -> Result<()> {
            fs::create_dir_all(actual.parent().unwrap())?;
            if actual.exists() {
                fs::remove_file(&actual)?;
            }
            let redirect = if job.kind.redirect() {
                Some(File::create(&actual)?)
            } else {
                None
            };
            let args = arguments(job, seed, &actual)?;
            record.command.extend(args.iter().cloned());
            let result = (self.backend)(Invocation {
                program: self.program,
                args: &args,
                cwd: self.work,
                redirect: redirect.as_ref(),
                timeout: self.timeout,
            })?;
            drop(redirect);
            record.timed_out = result.timed_out;
            record.returncode = process::return_code(result.status);
            if !job.kind.redirect() {
                for (name, bytes) in [
                    ("process_stdout", result.stdout),
                    ("process_stderr", result.stderr),
                ] {
                    let name_in_archive = format!("{}/{name}.txt", record.artifact_dir);
                    write_file(&self.out.join(&name_in_archive), &bytes)?;
                    if name == "process_stdout" {
                        record.process_stdout = Some(name_in_archive);
                    } else {
                        record.process_stderr = Some(name_in_archive);
                    }
                }
            }
            if result.timed_out {
                return Ok(());
            }
            let _lock = self
                .comparison_lock
                .lock()
                .map_err(|_| anyhow::anyhow!("comparison lock poisoned"))?;
            if record.returncode != Some(0) {
                // The original crash logger decodes piped streams as UTF-8.
                for path in [&record.process_stdout, &record.process_stderr]
                    .into_iter()
                    .flatten()
                {
                    let bytes = fs::read(self.out.join(path))?;
                    std::str::from_utf8(&bytes)?;
                }
                return Ok(());
            }
            let bytes = fs::read(&actual)?;
            if expected.exists() {
                record.comparison_matched = Some(compare(&bytes, &fs::read(&expected)?)?);
            } else {
                // Upstream creates an absent golden only in the disposable copy.
                // TextIO normalizes newlines; such a run remains unasserted.
                let text = text_newlines(std::str::from_utf8(&bytes)?);
                #[cfg(windows)]
                let text = text.replace('\n', "\r\n");
                write_file(&expected, text.as_bytes())?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            record.observer_error = Some(format!("{error:#}"));
        }
        // Freeze this seed's bytes before the next invocation truncates `actual`.
        let capture = (|| -> Result<()> {
            if actual.exists() {
                let bytes = fs::read(&actual)?;
                let path = format!("{}/actual.txt", record.artifact_dir);
                write_file(&self.out.join(&path), &bytes)?;
                record.actual_sha256 = Some(sha(&bytes));
                record.actual = Some(path);
            }
            Ok(())
        })();
        if let Err(error) = capture {
            record.observer_error = Some(format!("{error:#}"));
        }
        record.status = if record.observer_error.is_some() {
            "observer_error"
        } else if record.timed_out {
            "timeout"
        } else if record.returncode != Some(0) {
            "crash"
        } else if !record.golden_present {
            "unasserted"
        } else if record.comparison_matched == Some(true) {
            "pass"
        } else {
            "mismatch"
        };
        record.elapsed_seconds =
            (started.elapsed().as_secs_f64() * 1_000_000.).round() / 1_000_000.;
        record
    }
    fn execute(&self, jobs: &[Job]) -> Result<Vec<Record>> {
        let next = AtomicUsize::new(0);
        let results = Mutex::new(Vec::new());
        thread::scope(|scope| -> Result<()> {
            let mut workers = Vec::new();
            for _ in 0..WORKERS {
                workers.push(scope.spawn(|| {
                    while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        for seed in &job.seeds {
                            let record = self.observe(job, seed);
                            let stop = record.observer_error.is_some();
                            results.lock().unwrap().push(record);
                            if stop {
                                break;
                            } // runTestGeneric aborts remaining seeds on an exception.
                        }
                    }
                }));
            }
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("observer worker panicked"))?;
            }
            Ok(())
        })?;
        let mut results = results.into_inner().unwrap();
        results.sort_by(|a, b| (&a.fixture, &a.seed).cmp(&(&b.fixture, &b.seed)));
        Ok(results)
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(path.parent().context("file parent")?)?;
    fs::write(path, bytes)?;
    Ok(())
}
fn copy_snapshot(vendor: &Path, work: &Path) -> Result<()> {
    // Validate the complete tree before copying (no symlinks/nested .git).
    let source_files = files(vendor)?;
    fn directories(source: &Path, dest: &Path) -> Result<()> {
        fs::create_dir(dest)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories(&entry.path(), &dest.join(entry.file_name()))?;
            }
        }
        Ok(())
    }
    directories(vendor, work)?;
    for path in source_files {
        fs::copy(&path, work.join(path.strip_prefix(vendor)?))?;
    }
    Ok(())
}
fn drift(work: &Path, manifest: &Value) -> Result<(Vec<String>, Vec<String>)> {
    let pinned = manifest["files_sha256"]
        .as_object()
        .context("source manifest")?;
    let mut changed = Vec::new();
    for (name, digest) in pinned {
        if !work.join(name).is_file()
            || sha(&fs::read(work.join(name))?) != digest.as_str().context("source digest")?
        {
            changed.push(name.clone());
        }
    }
    let mut created = Vec::new();
    for directory in ["testData", "doc/examples"] {
        for path in files(&work.join(directory))? {
            let name = path
                .strip_prefix(work)?
                .to_str()
                .context("source path")?
                .replace('\\', "/");
            if !pinned.contains_key(&name) {
                created.push(name);
            }
        }
    }
    created.sort();
    Ok((changed, created))
}
fn archive(out: &Path, records: &mut [Record]) -> Result<()> {
    let mut zip = ZipWriter::new(File::create(out.join("upstream-artifacts.zip"))?);
    let raw = out.join("raw");
    if raw.exists() {
        for path in files(&raw)? {
            let name = path
                .strip_prefix(out)?
                .to_str()
                .context("archive path")?
                .replace('\\', "/");
            zip.start_file(
                name,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .last_modified_time(DateTime::default()),
            )?;
            zip.write_all(&fs::read(path)?)?;
        }
    }
    zip.finish()?;
    for record in records {
        if record.status != "pass"
            && let Some(actual) = &record.actual
        {
            let name = format!("failures/{}/{}.actual.txt", record.fixture, record.seed);
            write_file(&out.join(&name), &fs::read(out.join(actual))?)?;
            record.readable_actual = Some(name);
        }
    }
    if raw.exists() {
        fs::remove_dir_all(raw)?;
    }
    Ok(())
}
fn runner_exit(records: &[Record]) -> i32 {
    if records.iter().any(|r| r.observer_error.is_some()) {
        2
    } else if records
        .iter()
        .any(|r| matches!(r.status, "timeout" | "crash" | "mismatch"))
    {
        1
    } else {
        0
    } // Missing goldens count as passed upstream, but the bridge rejects drift.
}
fn bridge_exit(code: i32, changed: &[String], created: &[String]) -> i32 {
    if code == 2 {
        // The Python bridge recorded 2 before rethrowing observer exceptions;
        // Python's uncaught-exception process exit was 1, not the summary code.
        1
    } else if code != 0 {
        code
    } else {
        i32::from(!changed.is_empty() || !created.is_empty())
    }
}

fn new_artifact_path(path: &Path, base: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        !absolute
            .components()
            .any(|c| matches!(c, Component::ParentDir)),
        "output path must not contain .."
    );
    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        suffix.push(existing.file_name().context("output path")?.to_owned());
        existing = existing.parent().context("output ancestor")?;
    }
    let mut resolved = existing.canonicalize()?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    ensure!(
        resolved.starts_with(base) && resolved != base,
        "output/work must be below target/reel-artifacts"
    );
    ensure!(
        !resolved.exists(),
        "choose a new output/work directory: {}",
        resolved.display()
    );
    Ok(resolved)
}

pub fn run_cli(args: &[String]) -> Result<i32> {
    let mut options = BTreeMap::new();
    ensure!(
        args.len() == 6,
        "run-upstream --sq PATH --out NEW_DIR --work NEW_DIR"
    );
    for pair in args.chunks_exact(2) {
        ensure!(
            ["--sq", "--out", "--work"].contains(&pair[0].as_str()),
            "unknown run-upstream option"
        );
        ensure!(
            options.insert(pair[0].as_str(), pair[1].as_str()).is_none(),
            "duplicate option"
        );
    }
    let program = Path::new(options["--sq"]).canonicalize()?;
    ensure!(program.is_file(), "sq executable must be a file");
    let base = maintenance::artifacts()?.canonicalize()?;
    let out = new_artifact_path(Path::new(options["--out"]), &base)?;
    let work = new_artifact_path(Path::new(options["--work"]), &base)?;
    ensure!(
        !out.starts_with(&work) && !work.starts_with(&out),
        "out/work must not overlap"
    );
    let manifest = fixtures::snapshot()?;
    fixtures::conversions(false)?;
    fixtures::immutable_t01()?;
    let vendor = root().join("third_party/quirrel");
    Session {
        program: &program,
        vendor: &vendor,
        out: &out,
        work: &work,
        manifest: &manifest,
        timeout: TEST_TIMEOUT,
        backend: &native,
    }
    .run(|| fixtures::snapshot().map(|_| ()))
}

// Injection is limited to child execution and the source-pin check. Tests use
// small synthetic snapshots; the CLI always supplies the pinned vendor/native
// backend and checks the real snapshot before and after the disposable run.
struct Session<'a> {
    program: &'a Path,
    vendor: &'a Path,
    out: &'a Path,
    work: &'a Path,
    manifest: &'a Value,
    timeout: Duration,
    backend: &'a Backend<'a>,
}
impl Session<'_> {
    fn version_query(&self, name: &str) -> Result<process::Outcome> {
        let result = (self.backend)(Invocation {
            program: self.program,
            args: &["-v".to_owned()],
            cwd: self.work,
            redirect: None,
            timeout: self.timeout,
        })?;
        // Version queries were outside the legacy per-fixture records. Keep
        // their bytes too, including failed/timed-out version probes.
        write_file(
            &self.out.join(format!("versions/{name}.stdout")),
            &result.stdout,
        )?;
        write_file(
            &self.out.join(format!("versions/{name}.stderr")),
            &result.stderr,
        )?;
        Ok(result)
    }

    fn run(&self, snapshot_check: impl FnOnce() -> Result<()>) -> Result<i32> {
        let Self {
            program,
            vendor,
            out,
            work,
            manifest,
            timeout,
            backend,
        } = *self;
        let jobs = plan(vendor)?;
        fs::create_dir_all(out.parent().unwrap())?;
        fs::create_dir(out)?;
        fs::create_dir_all(work.parent().unwrap())?;
        copy_snapshot(vendor, work)?;
        let started = Instant::now();
        let version = self.version_query("startup");
        let (mut records, code, start_error) = match version {
            Ok(result) if result.status.success() && !result.timed_out => {
                let runner = Runner {
                    program,
                    work,
                    out,
                    timeout,
                    backend,
                    comparison_lock: Mutex::new(()),
                };
                let records = runner.execute(&jobs)?;
                let code = runner_exit(&records);
                (records, code, None)
            }
            Ok(_) => (
                Vec::new(),
                1,
                Some("compiler -v failed or timed out".to_owned()),
            ),
            Err(error) => (Vec::new(), 1, Some(format!("compiler -v: {error:#}"))),
        };
        let snapshot_check = snapshot_check();
        let (changed, created) = drift(work, manifest)?;
        // Match the second version query made by the original observer's finally block.
        let version = self.version_query("final");
        let (version_text, version_error) = match version {
            Ok(result) if result.status.success() && !result.timed_out => {
                match String::from_utf8(result.stdout) {
                    Ok(text) => (Some(text.trim_matches(python_space).to_owned()), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            }
            Ok(_) => (
                None,
                Some("final compiler -v failed or timed out".to_owned()),
            ),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        archive(out, &mut records)?;
        let mut summary = json!({"schema_version":1,"observer":"reel-dev Rust; pinned upstream runner contract","upstream_commit":manifest["commit"],"sq_sha256":sha(&fs::read(program)?),"sq_version":version_text,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"runner_exit_code":code,"threads":WORKERS,"timeout_seconds":timeout.as_secs(),"elapsed_seconds":started.elapsed().as_secs_f64(),"changed_snapshot_files_in_work":changed,"created_goldens_in_work":created,"artifact_archive":"upstream-artifacts.zip","runs":records});
        if let Some(error) = start_error {
            summary["startup_error"] = json!(error);
        }
        if let Some(error) = version_error {
            summary["version_error"] = json!(error);
        }
        if let Err(error) = &snapshot_check {
            summary["snapshot_error"] = json!(format!("{error:#}"));
        }
        fs::write(
            out.join("upstream.json"),
            serde_json::to_string_pretty(&summary)? + "\n",
        )?;
        let log = records
            .iter()
            .map(|r| {
                format!(
                    "{} {} seed={} returncode={:?} timeout={} command={:?}\n",
                    r.status, r.fixture, r.seed, r.returncode, r.timed_out, r.command
                )
            })
            .collect::<String>();
        fs::write(out.join("upstream.log"), log)?;
        println!(
            "runner_exit_code={code}; runs={}; output={}",
            records.len(),
            out.display()
        );
        snapshot_check?;
        if summary.get("version_error").is_some() {
            // subprocess.run(check=True) in the legacy finally block raised an
            // uncaught exception, even if the test runner itself had succeeded.
            return Ok(1);
        }
        Ok(bridge_exit(code, &changed, &created))
    }
}

#[cfg(test)]
mod tests;

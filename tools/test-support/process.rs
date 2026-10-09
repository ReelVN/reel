//! Bounded child execution shared by development tools and repository tests.
//! Captures bytes concurrently and includes pipe draining in the deadline.
use std::{
    fs::File,
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn drain(pipe: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut pipe = pipe;
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn collect(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    reader.map_or_else(
        || Ok(Vec::new()),
        |r| {
            r.join()
                .map_err(|_| io::Error::other("child output reader panicked"))?
        },
    )
}

/// Terminate the group established by `run`, including time/timeout wrappers.
/// No unrelated process is addressed: the group ID comes from our own child.
fn kill_tree(child: &mut Child) -> io::Result<()> {
    #[cfg(unix)]
    let result = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .output();
    #[cfg(windows)]
    let result = Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .output();
    #[cfg(not(any(unix, windows)))]
    let result: io::Result<std::process::Output> =
        Err(io::Error::other("process-tree termination unsupported"));
    match result {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::other(format!(
                "cannot terminate child tree: {}",
                String::from_utf8_lossy(&result.stderr)
            )))
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// Redirect both streams to the same file, or capture separate byte streams.
/// The timeout covers waiting for the child AND EOF on both output pipes.
pub fn run(
    command: &mut Command,
    timeout: Duration,
    redirect: Option<&File>,
) -> io::Result<Outcome> {
    if let Some(file) = redirect {
        command.stdout(file.try_clone()?).stderr(file.try_clone()?);
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let start = Instant::now();
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let mut status = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(result)) => status = Some(result),
            Ok(None) => {}
            Err(error) => {
                let _ = kill_tree(&mut child);
                return Err(error);
            }
        }
        if status.is_some()
            && stdout.as_ref().is_none_or(JoinHandle::is_finished)
            && stderr.as_ref().is_none_or(JoinHandle::is_finished)
        {
            break;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            if let Err(error) = kill_tree(&mut child) {
                // A completed group may disappear between the deadline and kill.
                // Only accept that race when both pipes have actually reached EOF.
                if !(child.try_wait()?.is_some()
                    && stdout.as_ref().is_none_or(JoinHandle::is_finished)
                    && stderr.as_ref().is_none_or(JoinHandle::is_finished))
                {
                    return Err(error);
                }
            }
            status = Some(child.wait()?);
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(Outcome {
        status: status.expect("child was reaped"),
        timed_out,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    })
}

/// Match Python's negative signal return codes on Unix.
pub fn return_code(status: ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .or_else(|| status.signal().map(|signal| -signal))
    }
    #[cfg(not(unix))]
    {
        status.code()
    }
}

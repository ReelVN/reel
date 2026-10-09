//! Bounded child execution shared by development tools and repository tests.
//! Captures bytes while polling the child, with bounded timeout cleanup.
//! Unix termination addresses our process group, not descendants which create
//! another group/session. Windows taskkill is best effort, not a Job Object.
use std::{
    fs::File,
    io::{self, Read},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const CLEANUP_GRACE: Duration = Duration::from_millis(250);
const TERMINATOR_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[cfg(unix)]
struct Capture(std::os::unix::net::UnixStream);

#[cfg(unix)]
impl Capture {
    fn new() -> io::Result<(Self, Stdio)> {
        use std::os::{fd::OwnedFd, unix::net::UnixStream};
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        let writer: OwnedFd = writer.into();
        Ok((Self(reader), Stdio::from(writer)))
    }

    fn read_available(
        &mut self,
        bytes: &mut Vec<u8>,
        deadline: Instant,
        _child_exited: bool,
    ) -> io::Result<bool> {
        let mut buffer = [0; 32 * 1024];
        // Check the child/deadline regularly even with continuously written output.
        for _ in 0..8 {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            match self.0.read(&mut buffer) {
                Ok(0) => return Ok(true),
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }
}

// Windows has no cancellable anonymous-pipe reader in std. Use independent
// regular-file readers for the supported synchronous tool wrappers instead of
// detaching blocking reader threads. Completion snapshots the byte length; it
// does not imply that an escaped descendant has closed every inherited handle.
// This backend/taskkill path has not been verified on Windows.
#[cfg(windows)]
struct Capture {
    reader: File,
    path: std::path::PathBuf,
    end: Option<u64>,
}

#[cfg(windows)]
impl Capture {
    fn new() -> io::Result<(Self, Stdio)> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "reel-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(writer) => {
                    let reader = match File::open(&path) {
                        Ok(reader) => reader,
                        Err(error) => {
                            drop(writer);
                            let _ = std::fs::remove_file(path);
                            return Err(error);
                        }
                    };
                    return Ok((
                        Self {
                            reader,
                            path,
                            end: None,
                        },
                        Stdio::from(writer),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn read_available(
        &mut self,
        bytes: &mut Vec<u8>,
        deadline: Instant,
        child_exited: bool,
    ) -> io::Result<bool> {
        let end = *self.end.get_or_insert_with(|| u64::MAX);
        if child_exited && end == u64::MAX {
            self.end = Some(self.reader.metadata()?.len());
        }
        let end = self.end.unwrap().min(self.reader.metadata()?.len());
        let mut buffer = [0; 32 * 1024];
        for _ in 0..8 {
            if Instant::now() >= deadline || bytes.len() as u64 >= end {
                break;
            }
            let count =
                usize::try_from((end - bytes.len() as u64).min(buffer.len() as u64)).unwrap();
            let count = self.reader.read(&mut buffer[..count])?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        Ok(child_exited && bytes.len() as u64 >= end)
    }
}

#[cfg(windows)]
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(any(unix, windows)))]
struct Capture;
#[cfg(not(any(unix, windows)))]
impl Capture {
    fn new() -> io::Result<(Self, Stdio)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bounded capture unsupported on this platform",
        ))
    }
    fn read_available(&mut self, _: &mut Vec<u8>, _: Instant, _: bool) -> io::Result<bool> {
        unreachable!("unsupported capture is rejected before spawning")
    }
}

fn drain(
    reader: &mut Option<Capture>,
    bytes: &mut Vec<u8>,
    deadline: Instant,
    child_exited: bool,
) -> io::Result<bool> {
    match reader {
        Some(reader) => reader.read_available(bytes, deadline, child_exited),
        None => Ok(true),
    }
}

fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(2));
    }
}

/// Request termination of our group/tree, without waiting indefinitely for the
/// utility or child. Escaped descendants are outside this termination boundary.
fn kill_tree(child: &mut Child, deadline: Instant) -> io::Result<()> {
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/kill");
        command.args(["-KILL", "--", &format!("-{}", child.id())]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &child.id().to_string(), "/T", "/F"]);
        command
    };
    #[cfg(not(any(unix, windows)))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process-tree termination unsupported",
    ));
    #[cfg(any(unix, windows))]
    {
        let mut terminator = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let limit = deadline.min(Instant::now() + TERMINATOR_GRACE);
        match wait_until(&mut terminator, limit)? {
            Some(status) if status.success() => Ok(()),
            Some(status) => Err(io::Error::other(format!("tree terminator exited {status}"))),
            None => {
                let _ = terminator.kill();
                let _ = wait_until(&mut terminator, deadline);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "tree terminator exceeded cleanup deadline",
                ))
            }
        }
    }
}

/// Redirect both streams to the same file, or capture separate byte streams.
/// Unix capture preserves EOF semantics using nonblocking local stream pairs.
/// The timeout covers the child and captured streams, followed by at most a
/// 250 ms cleanup window. No blocking reader thread/join survives this call.
/// Timeout output is the byte prefix captured before cleanup ends; escaped
/// descendants need not have reached EOF or been terminated.
pub fn run(
    command: &mut Command,
    timeout: Duration,
    redirect: Option<&File>,
) -> io::Result<Outcome> {
    let (mut stdout_reader, mut stderr_reader) = if let Some(file) = redirect {
        command.stdout(file.try_clone()?).stderr(file.try_clone()?);
        (None, None)
    } else {
        let (stdout, stdout_stdio) = Capture::new()?;
        let (stderr, stderr_stdio) = Capture::new()?;
        command.stdout(stdout_stdio).stderr(stderr_stdio);
        (Some(stdout), Some(stderr))
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let spawned = command.spawn();
    // Drop the parent's copies of stream writers; the child owns its duplicates.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = spawned?;
    let deadline = Instant::now() + timeout;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(result)) => status = Some(result),
            Ok(None) => {}
            Err(error) => {
                let cleanup = Instant::now() + CLEANUP_GRACE;
                let _ = kill_tree(&mut child, cleanup);
                let _ = child.kill();
                let _ = wait_until(&mut child, cleanup);
                return Err(error);
            }
        }
        let read = (|| {
            Ok::<_, io::Error>((
                drain(&mut stdout_reader, &mut stdout, deadline, status.is_some())?,
                drain(&mut stderr_reader, &mut stderr, deadline, status.is_some())?,
            ))
        })();
        let (stdout_done, stderr_done) = match read {
            Ok(done) => done,
            Err(error) => {
                let cleanup = Instant::now() + CLEANUP_GRACE;
                let _ = kill_tree(&mut child, cleanup);
                let _ = child.kill();
                let _ = wait_until(&mut child, cleanup);
                return Err(error);
            }
        };
        if status.is_some() && stdout_done && stderr_done {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let cleanup = Instant::now() + CLEANUP_GRACE;
            let termination = kill_tree(&mut child, cleanup);
            if termination.is_err() && status.is_none() {
                let _ = child.kill();
            }
            while Instant::now() < cleanup {
                if status.is_none() {
                    status = child.try_wait()?;
                }
                let stdout_done =
                    drain(&mut stdout_reader, &mut stdout, cleanup, status.is_some())?;
                let stderr_done =
                    drain(&mut stderr_reader, &mut stderr, cleanup, status.is_some())?;
                if status.is_some() && stdout_done && stderr_done {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }
            // One last nonblocking reap, then close capture endpoints even if an
            // escaped holder prevents EOF. Never fall back to wait()/join().
            if status.is_none() {
                status = child.try_wait()?;
            }
            if status.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "child did not exit within cleanup deadline; termination: {termination:?}"
                    ),
                ));
            }
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(Outcome {
        status: status.expect("child was reaped"),
        timed_out,
        stdout,
        stderr,
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

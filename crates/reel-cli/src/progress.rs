use std::{
    io::{self, IsTerminal, Write},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct State {
    label: &'static str,
    completed: usize,
    total: usize,
    stopped: bool,
}

/// One terminal line for source preparation. Redirected and JSON output stay quiet.
pub(crate) struct Progress {
    state: Option<Arc<(Mutex<State>, Condvar)>>,
    worker: Option<JoinHandle<()>>,
    started: Instant,
}

impl Progress {
    pub(crate) fn new(json: bool) -> Self {
        let started = Instant::now();
        let enabled = !json
            && io::stderr().is_terminal()
            && std::env::var_os("TERM").is_none_or(|term| term != "dumb");
        if !enabled {
            return Self {
                state: None,
                worker: None,
                started,
            };
        }
        let state = Arc::new((
            Mutex::new(State {
                label: "Reading project",
                completed: 0,
                total: 0,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let worker = thread::spawn(move || render(shared, started));
        Self {
            state: Some(state),
            worker: Some(worker),
            started,
        }
    }

    pub(crate) fn update(&self, label: &'static str, completed: usize, total: usize) {
        if let Some(shared) = &self.state {
            let mut state = shared.0.lock().unwrap();
            state.label = label;
            state.completed = completed;
            state.total = total;
        }
    }

    pub(crate) fn phase(&self, label: &'static str) {
        self.update(label, 0, 0);
    }

    pub(crate) fn stop(&mut self) {
        if let Some(shared) = &self.state {
            shared.0.lock().unwrap().stopped = true;
            shared.1.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    pub(crate) fn finish(&mut self, label: &str) {
        self.stop();
        if self.state.is_some() {
            let _ = writeln!(
                io::stderr().lock(),
                "{label} ({:.1}s)",
                self.started.elapsed().as_secs_f64()
            );
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop();
    }
}

fn line(state: State, tick: usize, elapsed: Duration) -> String {
    const WIDTH: usize = 20;
    let mut bar = [b' '; WIDTH];
    let count =
        if let Some(filled) = (state.completed.min(state.total) * WIDTH).checked_div(state.total) {
            bar[..filled].fill(b'=');
            if filled < WIDTH {
                bar[filled] = b'>';
            }
            format!(" {}/{}", state.completed, state.total)
        } else {
            let position = tick % (WIDTH - 2);
            bar[position..position + 3].fill(b'=');
            if state.completed == 0 {
                String::new()
            } else {
                format!(" {} modules", state.completed)
            }
        };
    format!(
        "[{}] {}{}  {:.1}s",
        std::str::from_utf8(&bar).unwrap(),
        state.label,
        count,
        elapsed.as_secs_f64()
    )
}

fn render(shared: Arc<(Mutex<State>, Condvar)>, started: Instant) {
    let mut tick = 0;
    let mut width = 0;
    loop {
        let state = *shared.0.lock().unwrap();
        if state.stopped {
            break;
        }
        let text = line(state, tick, started.elapsed());
        width = width.max(text.len());
        let mut output = io::stderr().lock();
        if write!(output, "\r{text:width$}")
            .and_then(|_| output.flush())
            .is_err()
        {
            return;
        }
        drop(output);
        tick += 1;
        let guard = shared.0.lock().unwrap();
        if guard.stopped {
            break;
        }
        let _ = shared
            .1
            .wait_timeout(guard, Duration::from_millis(100))
            .unwrap();
    }
    let mut output = io::stderr().lock();
    let _ = write!(output, "\r{:width$}\r", "").and_then(|_| output.flush());
}

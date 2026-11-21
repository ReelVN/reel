//! Desktop wake and render-completion observations on the recording timeline.
use crate::story_runtime::WakeSource;
use reel_render::RenderEvent;
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::{Arc, Mutex},
    time::Instant,
};
use winit::event::StartCause;

const CAPACITY: usize = 4096;

/// Why the desktop host selected this wait deadline.
#[derive(Clone, Copy)]
pub(crate) enum Reason {
    UploadPoll,
    CompletionPoll,
    Story(WakeSource),
    Input,
    Notified,
    Idle,
    Suspended,
    Shutdown,
}
impl Reason {
    fn name(self) -> &'static str {
        match self {
            Self::UploadPoll => "upload_poll",
            Self::CompletionPoll => "completion_poll",
            Self::Story(source) => match source {
                WakeSource::Ready => "story_ready",
                WakeSource::Maintenance => "story_maintenance",
                WakeSource::Save => "story_save",
                WakeSource::AssetRead => "story_asset_read",
                WakeSource::Transition => "story_transition",
                WakeSource::Dialogue => "dialogue",
                WakeSource::Activity => "story_activity",
                WakeSource::Media => "media",
                WakeSource::Localization => "localization",
                WakeSource::UiAnimation => "ui_animation",
                WakeSource::Capture => "capture",
            },
            Self::Input => "input",
            Self::Notified => "notified",
            Self::Idle => "idle",
            Self::Suspended => "suspended",
            Self::Shutdown => "shutdown",
        }
    }
}

/// A deadline applied in the prior desktop loop callback.
#[derive(Clone, Copy)]
pub(crate) struct Plan {
    pub(crate) at: Instant,
    pub(crate) deadline: Option<Instant>,
    pub(crate) reason: Reason,
}

enum Event {
    Wake {
        at: Instant,
        plan: Option<Plan>,
        cause: StartCause,
    },
    FrameCompleted {
        at: Instant,
        submission: u64,
        frame: u64,
    },
    ResourcesAvailable {
        at: Instant,
    },
}

#[derive(Default)]
struct Events {
    dropped: u64,
    rows: VecDeque<Event>,
}
impl Events {
    fn push(&mut self, event: Event) {
        if self.rows.capacity() == 0 {
            self.rows.reserve_exact(CAPACITY);
        }
        if self.rows.len() == CAPACITY {
            self.rows.pop_front();
            self.dropped += 1;
        }
        self.rows.push_back(event);
    }
}

/// Opt-in bounded observations shared by the desktop loop and player poll path.
#[derive(Clone)]
pub(crate) struct Trace {
    origin: Instant,
    events: Arc<Mutex<Events>>,
}
impl Trace {
    pub(super) fn new(origin: Instant) -> Self {
        Self {
            origin,
            events: Default::default(),
        }
    }

    pub(crate) fn wake(&self, at: Instant, plan: Option<Plan>, cause: StartCause) {
        self.events
            .lock()
            .unwrap()
            .push(Event::Wake { at, plan, cause });
    }

    /// Report when the player retrieved an event. This includes polling latency.
    pub(crate) fn completions<E>(&self, events: &[RenderEvent<E>]) {
        if events.is_empty() {
            return;
        }
        let at = Instant::now();
        let mut trace = self.events.lock().unwrap();
        for event in events {
            match event {
                RenderEvent::RenderFinished { id, frame } => trace.push(Event::FrameCompleted {
                    at,
                    submission: id.0,
                    frame: frame.0,
                }),
                RenderEvent::ResourcesAvailable => trace.push(Event::ResourcesAvailable { at }),
                _ => {}
            }
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.events.lock().unwrap().rows.capacity() * size_of::<Event>()
    }

    pub(super) fn report(&self, out: &mut impl Write) -> io::Result<()> {
        let trace = self.events.lock().unwrap();
        writeln!(
            out,
            "desktop_pacing_metadata\tversion=1\tclock=frame_elapsed_us\tcapacity={CAPACITY}\tdropped={}",
            trace.dropped
        )?;
        writeln!(
            out,
            "desktop_wake_columns\tobserved_us\treason\tplanned_at_us\tplanned_deadline_us\tcause\tnative_wait_start_us\tnative_deadline_us\ttimer_late_us"
        )?;
        writeln!(
            out,
            "desktop_completion_columns\tobserved_us\tkind\tsubmission_id\tframe_id"
        )?;
        for row in &trace.rows {
            match *row {
                Event::Wake { at, plan, cause } => {
                    let (name, start, requested) = match cause {
                        StartCause::ResumeTimeReached {
                            start,
                            requested_resume,
                        } => ("deadline", Some(start), Some(requested_resume)),
                        StartCause::WaitCancelled {
                            start,
                            requested_resume,
                        } => ("event", Some(start), requested_resume),
                        StartCause::Poll => ("poll", None, None),
                        StartCause::Init => ("init", None, None),
                    };
                    let late = match cause {
                        StartCause::ResumeTimeReached {
                            requested_resume, ..
                        } => Some(at.saturating_duration_since(requested_resume).as_micros()),
                        _ => None,
                    };
                    write!(out, "desktop_wake\t")?;
                    self.timestamp(out, Some(at))?;
                    write!(out, "\t{}\t", plan.map_or("-", |plan| plan.reason.name()))?;
                    self.timestamp(out, plan.map(|plan| plan.at))?;
                    write!(out, "\t")?;
                    self.timestamp(out, plan.and_then(|plan| plan.deadline))?;
                    write!(out, "\t{name}\t")?;
                    self.timestamp(out, start)?;
                    write!(out, "\t")?;
                    self.timestamp(out, requested)?;
                    match late {
                        Some(us) => writeln!(out, "\t{us}")?,
                        None => writeln!(out, "\t-")?,
                    }
                }
                Event::FrameCompleted {
                    at,
                    submission,
                    frame,
                } => {
                    write!(out, "desktop_completion\t")?;
                    self.timestamp(out, Some(at))?;
                    writeln!(out, "\tframe\t{submission}\t{frame}")?;
                }
                Event::ResourcesAvailable { at } => {
                    write!(out, "desktop_completion\t")?;
                    self.timestamp(out, Some(at))?;
                    writeln!(out, "\tresources\t-\t-")?;
                }
            }
        }
        Ok(())
    }

    fn timestamp(&self, out: &mut impl Write, value: Option<Instant>) -> io::Result<()> {
        match value.and_then(|time| time.checked_duration_since(self.origin)) {
            Some(duration) => write!(out, "{}", duration.as_micros()),
            None => write!(out, "-"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn deadline_and_event_wakes_distinguish_requested_from_actual_time() {
        let origin = Instant::now();
        let trace = Trace::new(origin);
        let at = |us| origin + Duration::from_micros(us);
        let plan = Plan {
            at: at(100),
            deadline: Some(at(1_100)),
            reason: Reason::Story(WakeSource::Dialogue),
        };
        trace.wake(
            at(1_140),
            Some(plan),
            StartCause::ResumeTimeReached {
                start: at(110),
                requested_resume: at(1_100),
            },
        );
        trace.wake(
            at(1_150),
            Some(Plan {
                deadline: Some(at(1_200)),
                ..plan
            }),
            StartCause::WaitCancelled {
                start: at(1_141),
                requested_resume: Some(at(1_200)),
            },
        );
        let mut out = Vec::new();
        trace.report(&mut out).unwrap();
        let report = String::from_utf8(out).unwrap();
        assert!(
            report.contains("desktop_wake\t1140\tdialogue\t100\t1100\tdeadline\t110\t1100\t40\n")
        );
        assert!(report.contains("desktop_wake\t1150\tdialogue\t100\t1200\tevent\t1141\t1200\t-\n"));
    }
}

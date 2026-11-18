use super::*;
use std::{sync::mpsc, thread};

// A paused WASAPI stream still contains endpoint samples from the previous
// session. Only the native stream crosses threads. The new mixer stays with
// its caller until the player accepts it on a clean endpoint.
pub(super) struct StreamRestart {
    completion: mpsc::Receiver<(CpalOutput, Result<(), AudioError>)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StreamRestart {
    fn begin(
        mut output: CpalOutput,
        format: Option<AudioFormat>,
        wake: WakeSignal,
    ) -> Result<Self, AudioError> {
        let (finished, completion) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("reel-audio-reopen".into())
            .spawn(move || {
                let result = output.inner.replace_parked_stream(format.as_ref());
                let _ = finished.send((output, result));
                wake.wake();
            })
            .map_err(|error| AudioError::new(AudioErrorKind::Unavailable, error.to_string()))?;
        Ok(Self {
            completion,
            thread: Some(thread),
        })
    }
}

impl Drop for StreamRestart {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Device {
    pub(super) fn prepare_attachment(
        &mut self,
        expected: &AudioFormat,
        wake: &WakeSignal,
    ) -> Result<(), AudioError> {
        if let Some(job) = &self.restart {
            match job.completion.try_recv() {
                Ok((output, result)) => {
                    self.restart = None;
                    self.output = Some(output);
                    if let Err(error) = result {
                        self.format = None;
                        if error.kind != AudioErrorKind::ReconfigureRequired {
                            return Err(error);
                        }
                    }
                    return Ok(());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(AudioError::new(
                        AudioErrorKind::DeviceFailure,
                        "audio endpoint worker stopped before completion",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else {
            if self.ready().status().state != AudioState::Suspended {
                return Ok(());
            }
            let output = self.output.take().expect("parked audio endpoint");
            self.restart = Some(StreamRestart::begin(
                output,
                self.format
                    .as_ref()
                    .filter(|format| *format == expected)
                    .cloned(),
                wake.clone(),
            )?);
        }
        Err(AudioError::new(
            AudioErrorKind::Busy,
            "preparing the new audio stream",
        ))
    }
}

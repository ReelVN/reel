use super::*;
use reel_core::result::{RejectCode, RejectReason};
use std::{cell::RefCell, rc::Rc};

#[cfg(target_os = "windows")]
mod restart;

/// Owns the desktop audio device and its configuration across story sessions.
#[derive(Clone)]
pub struct DesktopAudio {
    device: Rc<RefCell<Option<Device>>>,
    wake: WakeSignal,
}

struct Device {
    output: Option<CpalOutput>,
    capabilities: AudioCaps,
    #[cfg(target_os = "windows")]
    restart: Option<restart::StreamRestart>,
    format: Option<AudioFormat>,
    attached: bool,
}

impl DesktopAudio {
    /// Create a player-owned audio endpoint. The first session prepares the device.
    pub fn new(wake: WakeSignal) -> Self {
        Self {
            device: Rc::new(RefCell::new(None)),
            wake,
        }
    }

    /// Prepare a session's mixer independently. The output accepts only one fill at a time.
    pub fn session(&self) -> Result<impl AudioOutput + use<>, AudioError> {
        let capabilities = self
            .open(&mut self.device.borrow_mut())?
            .capabilities
            .clone();
        Ok(SessionOutput {
            desktop: self.clone(),
            capabilities,
            configuration: None,
            attached: false,
        })
    }

    fn open<'a>(&self, device: &'a mut Option<Device>) -> Result<&'a mut Device, AudioError> {
        if device.is_none() {
            let output = CpalOutput::new(self.wake.clone(), MonoTime(0))?;
            *device = Some(Device {
                capabilities: output.capabilities(),
                output: Some(output),
                #[cfg(target_os = "windows")]
                restart: None,
                format: None,
                attached: false,
            });
        }
        Ok(device.as_mut().unwrap())
    }
}

struct SessionOutput {
    desktop: DesktopAudio,
    capabilities: AudioCaps,
    configuration: Option<(AudioRequest, AudioFormat)>,
    attached: bool,
}

impl Device {
    fn ready(&mut self) -> &mut CpalOutput {
        self.output
            .as_mut()
            .expect("audio endpoint on control thread")
    }

    fn prepare(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError> {
        self.format = None;
        let format = self.ready().configure(request)?;
        self.capabilities = self.ready().capabilities();
        self.format = Some(format.clone());
        Ok(format)
    }

    fn start(
        &mut self,
        request: &AudioRequest,
        expected: &AudioFormat,
        fill: PcmFill,
    ) -> Result<(), Rejected<PcmFill>> {
        let fill = if self.format.as_ref() == Some(expected) {
            let output = self.ready();
            let parked = output.status().state == AudioState::Suspended;
            let result = if parked {
                output.inner.start_parked(fill)
            } else if cfg!(target_os = "windows") {
                output.inner.try_start(fill)
            } else {
                return output.start(fill);
            };
            match result {
                Ok(()) => return Ok(()),
                Err((error, fill)) if error.kind == AudioErrorKind::ReconfigureRequired => fill,
                Err((error, request)) if parked => {
                    return Err(Rejected {
                        request,
                        reason: RejectReason::new(RejectCode::ServiceFault, error.message),
                    });
                }
                Err((error, fill)) => {
                    return Err(DrivenOutput::<CpalDriver>::reject(fill, error));
                }
            }
        } else {
            fill
        };
        let _ = self.ready().stop();
        match self.prepare(request.clone()) {
            Ok(actual) if actual == *expected => self.ready().start(fill),
            Ok(_) => Err(Rejected {
                request: fill,
                reason: RejectReason::new(
                    RejectCode::Unsupported,
                    "audio format changed during session preparation",
                ),
            }),
            Err(error) => Err(Rejected {
                request: fill,
                reason: RejectReason::new(RejectCode::Unavailable, error.message),
            }),
        }
    }
}

impl AudioOutput for SessionOutput {
    fn capabilities(&self) -> AudioCaps {
        self.capabilities.clone()
    }

    fn configure(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError> {
        if self.attached {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "configure requires stop",
            ));
        }
        self.configuration = None;
        let mut shared = self.desktop.device.borrow_mut();
        let device = self.desktop.open(&mut shared)?;
        let format = if device.format.is_none() {
            device.prepare(request.clone())?
        } else {
            negotiate(&device.capabilities, &request)?
        };
        self.capabilities = device.capabilities.clone();
        self.configuration = Some((request, format.clone()));
        Ok(format)
    }

    fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>> {
        let Some((request, format)) = &self.configuration else {
            return Err(Rejected {
                request: fill,
                reason: RejectReason::new(RejectCode::InvalidRequest, "configure before start"),
            });
        };
        let mut shared = self.desktop.device.borrow_mut();
        let device = match self.desktop.open(&mut shared) {
            Ok(device) => device,
            Err(error) => {
                return Err(Rejected {
                    request: fill,
                    reason: RejectReason::new(RejectCode::Unavailable, error.message),
                });
            }
        };
        if device.attached {
            return Err(Rejected {
                request: fill,
                reason: RejectReason::new(RejectCode::Busy, "audio output has a session"),
            });
        }
        #[cfg(target_os = "windows")]
        if let Err(error) = device.prepare_attachment(format, &self.desktop.wake) {
            if error.kind != AudioErrorKind::Busy {
                *shared = None;
            }
            return Err(DrivenOutput::<CpalDriver>::reject(fill, error));
        }
        if let Err(error) = device.start(request, format, fill) {
            if device.ready().status().state == AudioState::Failed {
                *shared = None;
            }
            return Err(error);
        }
        device.attached = true;
        self.attached = true;
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), AudioError> {
        self.attached_device(|output| output.suspend())
    }

    fn resume(&mut self) -> Result<(), AudioError> {
        self.attached_device(|output| output.resume())
    }

    fn status(&self) -> AudioStatus {
        if self.attached {
            return self
                .desktop
                .device
                .borrow()
                .as_ref()
                .and_then(|device| device.output.as_ref())
                .expect("attached audio endpoint")
                .status();
        }
        AudioStatus {
            state: AudioState::Stopped,
            output_epoch: OutputEpoch::default(),
            underrun_count: 0,
            silent_frames: 0,
            realtime_denied_count: 0,
            latest_device_error: None,
        }
    }

    fn clock_observation(&self) -> AudioClockObservation {
        if self.attached {
            return self
                .desktop
                .device
                .borrow()
                .as_ref()
                .and_then(|device| device.output.as_ref())
                .expect("attached audio endpoint")
                .clock_observation();
        }
        let rate = self
            .configuration
            .as_ref()
            .map_or(0, |(_, format)| format.sample_rate_hz);
        AudioClockObservation::unavailable(OutputEpoch::default(), rate)
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.attached {
            return Ok(());
        }
        self.attached = false;
        let mut shared = self.desktop.device.borrow_mut();
        let device = shared.as_mut().unwrap();
        device.attached = false;
        let result = device.ready().inner.park();
        if result.is_err() {
            *shared = None;
        }
        result
    }
}

impl SessionOutput {
    fn attached_device(
        &mut self,
        action: impl FnOnce(&mut CpalOutput) -> Result<(), AudioError>,
    ) -> Result<(), AudioError> {
        if !self.attached {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "session has no audio output",
            ));
        }
        action(self.desktop.device.borrow_mut().as_mut().unwrap().ready())
    }
}

impl Drop for SessionOutput {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

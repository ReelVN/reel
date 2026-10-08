use crate::{AudioError, AudioErrorKind, AudioFormat, PcmFill};
use kira::{
    AudioManager, AudioManagerSettings, Frame, Tween,
    backend::{Backend, Renderer},
    info::Info,
    sound::{
        PlaybackState, Sound, SoundData,
        static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
    },
    track::MainTrackBuilder,
};
use reel_core::{
    budget::BudgetTicket,
    result::{RejectCode, RejectReason, Rejected},
};
use std::{sync::Arc, time::Duration};

struct Bridge {
    sample_rate: u32,
    fill: Option<PcmFill>,
}
impl Backend for Bridge {
    type Settings = u32;
    type Error = AudioError;
    fn setup(sample_rate: u32, _: usize) -> Result<(Self, u32), AudioError> {
        Ok((
            Self {
                sample_rate,
                fill: None,
            },
            sample_rate,
        ))
    }
    fn start(&mut self, mut renderer: Renderer) -> Result<(), AudioError> {
        debug_assert_ne!(self.sample_rate, 0);
        self.fill = Some(Box::new(move |out| {
            renderer.on_start_processing();
            renderer.process(out, 2);
        }));
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MixerLimits {
    pub max_sounds: usize,
    pub internal_buffer_frames: usize,
}
impl Default for MixerLimits {
    fn default() -> Self {
        Self {
            max_sounds: 64,
            internal_buffer_frames: 128,
        }
    }
}

/// Predecoded samples and their residency reservation. Does no file IO.
/// Ticket covers input capacity + converted Frame storage during construction,
/// and remains conservatively charged until Kira's control-side reclamation.
#[derive(Debug)]
pub struct DecodedSound {
    data: StaticSoundData,
    budget: Arc<BudgetTicket>,
}
pub type DecodedSoundRequest = (u32, Vec<[f32; 2]>, BudgetTicket);
impl DecodedSound {
    pub fn new(
        sample_rate_hz: u32,
        frames: Vec<[f32; 2]>,
        ticket: BudgetTicket,
    ) -> Result<Self, Rejected<DecodedSoundRequest>> {
        let required = (frames.capacity() as u64)
            .checked_add(frames.len() as u64)
            .and_then(|n| n.checked_mul(8));
        if sample_rate_hz == 0
            || frames.is_empty()
            || frames.len() > 8_388_608
            || required.is_none_or(|n| n > ticket.bytes())
        {
            return Err(Rejected {
                request: (sample_rate_hz, frames, ticket),
                reason: RejectReason::new(
                    RejectCode::InvalidRequest,
                    "invalid PCM or ticket does not cover input and converted storage",
                ),
            });
        }
        let data = StaticSoundData {
            sample_rate: sample_rate_hz,
            frames: frames
                .into_iter()
                .map(|[left, right]| Frame::new(left, right))
                .collect(),
            settings: StaticSoundSettings::default(),
            slice: None,
        };
        Ok(Self {
            data,
            budget: Arc::new(ticket),
        })
    }
    /// Loop the decoded region using Kira's transport, without duplicating PCM.
    pub fn looping(mut self) -> Self {
        self.data = self.data.loop_region(..);
        self
    }
    pub fn resident_bytes(&self) -> u64 {
        self.budget.bytes()
    }
}
struct BudgetedData {
    data: StaticSoundData,
    budget: Arc<BudgetTicket>,
}
struct BudgetedSound {
    sound: Box<dyn Sound>,
    _budget: Arc<BudgetTicket>,
}
impl Sound for BudgetedSound {
    fn on_start_processing(&mut self) {
        self.sound.on_start_processing();
    }
    fn process(&mut self, out: &mut [Frame], dt: f64, info: &Info) {
        self.sound.process(out, dt, info);
    }
    fn finished(&self) -> bool {
        self.sound.finished()
    }
}
impl SoundData for BudgetedData {
    type Error = ();
    type Handle = StaticSoundHandle;
    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), ()> {
        let (sound, handle) = self.data.into_sound()?;
        Ok((
            Box::new(BudgetedSound {
                sound,
                _budget: self.budget,
            }),
            handle,
        ))
    }
}

/// Kira native handles stay private to this adapter. Capacity and all control
/// allocations are established before real-time consumption starts.
pub struct KiraMixer {
    manager: AudioManager<Bridge>,
}
impl KiraMixer {
    pub fn new(format: &AudioFormat, limits: MixerLimits) -> Result<Self, AudioError> {
        format.validate()?;
        if !(1..=256).contains(&limits.max_sounds)
            || !(1..=4096).contains(&limits.internal_buffer_frames)
        {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "invalid Kira resource/buffer limits",
            ));
        }
        let manager = AudioManager::<Bridge>::new(AudioManagerSettings {
            backend_settings: format.sample_rate_hz,
            capacities: kira::Capacities {
                sub_track_capacity: 0,
                send_track_capacity: 0,
                clock_capacity: 0,
                modulator_capacity: 0,
                listener_capacity: 0,
            },
            main_track_builder: MainTrackBuilder::new().sound_capacity(limits.max_sounds),
            internal_buffer_size: limits.internal_buffer_frames,
        })?;
        Ok(Self { manager })
    }
    /// Linear transfer, at most once. Keep this manager alive while the output owns fill.
    pub fn take_fill(&mut self) -> Option<PcmFill> {
        self.manager.backend_mut().fill.take()
    }
    pub fn play(&mut self, sound: DecodedSound) -> Result<Playback, Rejected<DecodedSound>> {
        // Retain the exact request through Kira's fallible play; clone only immutable refs.
        let data = BudgetedData {
            data: sound.data.clone(),
            budget: sound.budget.clone(),
        };
        match self.manager.play(data) {
            Ok(handle) => Ok(Playback { handle }),
            Err(_) => Err(Rejected {
                request: sound,
                reason: RejectReason::new(RejectCode::Busy, "Kira sound capacity exhausted"),
            }),
        }
    }
}
/// Control wrapper; state/position describe content consumption, not device presentation.
pub struct Playback {
    handle: StaticSoundHandle,
}
impl Playback {
    pub fn position_seconds(&self) -> f64 {
        self.handle.position()
    }
    pub fn is_stopped(&self) -> bool {
        self.handle.state() == PlaybackState::Stopped
    }
    /// Coalesced Kira command slots; never competes for the bounded play queue.
    pub fn set_volume_db(&mut self, db: f32) -> Result<(), AudioError> {
        if !db.is_finite() {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "volume must be finite",
            ));
        }
        self.handle.set_volume(db, immediate());
        Ok(())
    }
    pub fn stop(&mut self) {
        self.handle.stop(immediate());
    }
}
fn immediate() -> Tween {
    Tween {
        duration: Duration::ZERO,
        ..Tween::default()
    }
}

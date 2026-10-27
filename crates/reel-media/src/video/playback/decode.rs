//! Per-track queue admission and worker-owned partial PCM publication.
use super::{
    PcmResidency, ResidentFrame, Sample, Shared, StreamSoundData, VideoOptions, VideoTerminal,
};
use crate::video::{AudioFrames, DecodeDemand, DecodedPacket, MediaRead, VideoDecoderFactory};
use reel_audio::Frame;
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::{
    sync::{Arc, atomic::Ordering},
    task::Poll,
    time::Duration,
};

struct PendingAudio {
    packet: AudioFrames,
    index: usize,
    _storage: BudgetTicket,
}
impl PendingAudio {
    /// Publish the prefix that fits, preserving the remainder and its residency.
    fn publish(
        &mut self,
        producer: &mut rtrb::Producer<Sample>,
        position: &mut f64,
        end: f64,
    ) -> bool {
        let step = 1. / f64::from(self.packet.sample_rate);
        loop {
            if *position + step / 2. >= end {
                return true;
            }
            let silent = *position + step / 2. < self.packet.pts;
            if !silent && self.index == self.packet.frames.len() {
                return true;
            }
            let frame = if silent {
                Frame::ZERO
            } else {
                let [left, right] = self.packet.frames[self.index];
                Frame::new(left, right)
            };
            if producer
                .push(Sample {
                    frame,
                    end: *position + step,
                })
                .is_err()
            {
                return false;
            }
            *position += step;
            if !silent {
                self.index += 1;
            }
        }
    }
}

pub(super) fn decode(
    source: Box<dyn MediaRead>,
    options: VideoOptions,
    budget: BudgetPool,
    shared: &Shared,
    factory: &dyn VideoDecoderFactory,
) -> Result<(), String> {
    let mut decoder = {
        let _profile = reel_core::profile::Scope::new(reel_core::profile::Phase::VideoOpen);
        factory.open(source, budget.clone(), shared.transport.cancelled.clone())?
    };
    if let Some(end) = options.end {
        decoder.set_end(end.as_secs_f64());
    }
    if options.poster {
        decoder.prepare_poster(options.start.as_secs_f64())?;
    } else if options.start != Duration::ZERO {
        decoder.seek(options.start.as_secs_f64())?;
    }
    let info = decoder.info().clone();
    let mut audio = if let Some(rate) = info.audio_sample_rate {
        let ticket = budget
            .reserve((options.queued_audio_frames * std::mem::size_of::<Sample>()) as u64)
            .map_err(|e| e.message)?;
        let residency = Arc::new(PcmResidency { _ticket: ticket });
        let (producer, consumer) = rtrb::RingBuffer::new(options.queued_audio_frames);
        shared.state.lock().unwrap().audio = Some(StreamSoundData {
            consumer,
            transport: shared.transport.clone(),
            sample_rate: rate,
            _residency: residency.clone(),
        });
        Some((producer, residency, options.start.as_secs_f64()))
    } else {
        None
    };
    shared.state.lock().unwrap().info = Some(info.clone());
    shared.wake.wake();
    let plane_bytes = u64::from(info.width) * u64::from(info.height)
        + 2 * u64::from(info.width.div_ceil(2)) * u64::from(info.height.div_ceil(2));
    let end = options.end.map_or(f64::INFINITY, |e| e.as_secs_f64());
    let mut pending: Option<PendingAudio> = None;
    let mut eof = false;
    let mut paused = false;
    loop {
        if shared.transport.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(packet) = pending.as_mut() {
            let (producer, _, position) = audio.as_mut().unwrap();
            if packet.publish(producer, position, end) {
                let returned = pending.take().unwrap();
                decoder.recycle_audio(returned.packet, returned._storage);
            }
        }
        if pending.is_none() && decoder.audio_finished() {
            shared.transport.audio_end.store(true, Ordering::Release);
        }
        if eof && pending.is_none() {
            return Ok(());
        }
        let demand = DecodeDemand {
            video: !eof && shared.state.lock().unwrap().frames.len() < options.queued_video_frames,
            audio: !eof
                && pending.is_none()
                && !decoder.audio_finished()
                && audio
                    .as_ref()
                    .is_some_and(|(producer, _, _)| !producer.is_full()),
        };
        let blocked = !demand.video && !demand.audio;
        if paused != blocked {
            decoder.set_paused(blocked)?;
            paused = blocked;
        }
        if blocked {
            if !eof {
                match decoder.poll_packet(demand)? {
                    Poll::Ready(None) => {
                        if options.poster {
                            return Err("video contains no picture at the saved position".into());
                        }
                        eof = true;
                        continue;
                    }
                    Poll::Ready(Some(_)) => {
                        return Err("video decoder returned an unrequested packet".into());
                    }
                    Poll::Pending => {}
                }
            }
            // PCM consumption is lock-free and does not signal this event.
            // Picture retirement and control changes provide retained wakes.
            let timeout = audio.as_ref().map(|_| Duration::from_millis(5));
            shared.available.wait(timeout)?;
            continue;
        }
        // Only requested output needs a reservation. A retained partial PCM
        // packet keeps its own ticket while a picture is being decoded.
        let capacity =
            if demand.video { plane_bytes } else { 0 }.max(if demand.audio { 1024 * 8 } else { 0 });
        let mut storage = budget.reserve(capacity).map_err(|e| e.message)?;
        let packet = {
            let _profile = reel_core::profile::Scope::new(reel_core::profile::Phase::VideoDecode);
            decoder.poll_packet(demand)?
        };
        match packet {
            Poll::Pending => {
                drop(storage);
                shared.available.wait(Some(Duration::from_millis(2)))?;
            }
            Poll::Ready(None) => {
                if options.poster {
                    return Err("video contains no picture at the saved position".into());
                }
                eof = true;
            }
            Poll::Ready(Some(DecodedPacket::Video(mut frame))) => {
                if !demand.video {
                    return Err("video decoder returned an unrequested picture".into());
                }
                if frame.pts >= end {
                    continue;
                }
                frame.duration = frame.duration.min(end - frame.pts);
                storage
                    .resize(frame.planes.iter().map(|p| p.capacity() as u64).sum())
                    .map_err(|e| e.message)?;
                let mut state = shared.state.lock().unwrap();
                state.frames.push_back(Arc::new(ResidentFrame {
                    frame,
                    _ticket: storage,
                }));
                if options.poster {
                    state.terminal = Some(VideoTerminal::Completed);
                }
                drop(state);
                shared.wake.wake();
                if options.poster {
                    return Ok(());
                }
            }
            Poll::Ready(Some(DecodedPacket::Audio(packet))) => {
                if !demand.audio || Some(packet.sample_rate) != info.audio_sample_rate {
                    return Err("video decoder returned unexpected PCM".into());
                }
                storage
                    .resize((packet.frames.capacity() * std::mem::size_of::<[f32; 2]>()) as u64)
                    .map_err(|e| e.message)?;
                pending = Some(PendingAudio {
                    packet,
                    index: 0,
                    _storage: storage,
                });
            }
        }
    }
}

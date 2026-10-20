use reel_core::{
    audio::AudioLoopRegion,
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    wake::WakeSignal,
};
use reel_media::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn timing(n: u64) -> CallbackTiming {
    CallbackTiming {
        now: MonoTime(n),
        latency_frames: Some(2),
        uncertainty_frames: Some(1),
    }
}
fn setup(
    layout: ChannelLayout,
    sample_format: SampleFormat,
    max_pull_frames: u32,
) -> (OfflineOutput, AudioFormat) {
    let mut output = OfflineOutput::new(
        AudioDeviceConfig {
            sample_rate_hz: 48_000,
            sample_format,
            layout,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let format = output
        .configure(AudioRequest {
            allowed_layouts: vec![layout],
            allowed_sample_formats: vec![sample_format],
            max_pull_frames,
            ..AudioRequest::default()
        })
        .unwrap();
    (output, format)
}
fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 8,
            max_bytes: 4 * 1024 * 1024,
        },
    )
}
fn sound(pool: &BudgetPool, samples: Vec<[f32; 2]>) -> DecodedSound {
    let ticket = pool
        .reserve((samples.capacity() + samples.len()) as u64 * 8)
        .unwrap();
    DecodedSound::new(48_000, samples, ticket).unwrap()
}

fn stereo_tail(output: &mut OfflineOutput) -> [f32; 2] {
    let mut pcm = [0.0_f32; 256];
    output.render(&mut pcm, timing(0));
    [pcm[254], pcm[255]]
}
fn near(actual: [f32; 2], expected: [f32; 2]) {
    for (a, b) in actual.into_iter().zip(expected) {
        assert!((a - b).abs() < 0.0001, "{actual:?} != {expected:?}");
    }
}

#[test]
fn playback_end_matches_cropped_pcm_through_resampling_and_queued_loop_exit() {
    use std::time::Duration;
    fn render(source_rate: u32, output_rate: u32, sliced: bool, looping: bool) -> Vec<f32> {
        let mut service =
            AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
        let mut output = OfflineOutput::new(
            AudioDeviceConfig {
                sample_rate_hz: output_rate,
                sample_format: SampleFormat::F32,
                layout: ChannelLayout::Stereo,
            },
            WakeSignal::new(|| {}),
        )
        .unwrap();
        let format = output.configure(AudioRequest::default()).unwrap();
        let mut mixer = KiraMixer::new(
            &format,
            MixerLimits {
                max_sounds: 4,
                internal_buffer_frames: 64,
            },
            &service,
        )
        .unwrap();
        let pool = pool();
        let time = |frame: usize| Duration::from_secs_f64(frame as f64 / f64::from(source_rate));
        let make = |start: usize, end: usize, amplitude: f32, looping: bool| {
            // The excluded suffix is deliberately loud and unlike either clip.
            let mut frames = vec![[amplitude, -amplitude]; end];
            if sliced {
                frames.extend(std::iter::repeat_n([0.99, 0.98], 200));
            }
            let ticket = pool
                .reserve((frames.capacity() + frames.len()) as u64 * 8)
                .unwrap();
            DecodedSound::new(source_rate, frames, ticket)
                .unwrap()
                .with_playback(
                    time(start),
                    sliced.then(|| time(end)),
                    looping.then_some(AudioLoopRegion {
                        start: time(60),
                        end: None,
                    }),
                )
                .unwrap()
        };
        let mut playback = mixer.play(make(40, 160, 0.2, looping)).unwrap();
        mixer.announce(&playback, 1).unwrap();
        mixer
            .queue(&mut playback, 1, make(20, 120, 0.4, false))
            .unwrap();
        assert!(output.start(mixer.take_fill().unwrap()).is_ok());
        let mut pcm = vec![0.; 2048];
        output.render(&mut pcm, timing(0));
        assert!(playback.is_stopped());
        assert_eq!(playback.queue_cursor().0, 1);
        assert!((playback.position_seconds() - time(120).as_secs_f64()).abs() < 1e-8);
        if source_rate == output_rate {
            for (index, frame) in pcm[..440].chunks_exact(2).enumerate() {
                let amplitude = if index < 120 { 0.2 } else { 0.4 };
                near([frame[0], frame[1]], [amplitude, -amplitude]);
            }
        }
        output.stop().unwrap();
        drop(mixer);
        service.drain_and_join().unwrap();
        assert_eq!(pool.usage().bytes, 0);
        pcm
    }
    for (source, output) in [(48_000, 48_000), (44_100, 48_000), (48_000, 44_100)] {
        for looping in [false, true] {
            assert_eq!(
                render(source, output, true, looping),
                render(source, output, false, looping),
                "slice must retain the same sample history at {source}->{output}, loop={looping}"
            );
        }
    }
}

#[test]
fn playback_end_quantized_empty_segment_and_restored_endpoint_complete_silently() {
    use std::time::Duration;
    let time = |frame: f64| Duration::from_secs_f64(frame / 48_000.);
    // Initial times differ but round to the same source frame; the second case
    // restores an already reached endpoint rather than a new author play call.
    for (start, end) in [(time(0.9), time(1.1)), (time(1.), time(1.))] {
        let pool = pool();
        let mut service =
            AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
        let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 128);
        let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
        let playback = mixer
            .play(
                sound(&pool, vec![[0.9; 2]; 64])
                    .with_playback(start, Some(end), None)
                    .unwrap(),
            )
            .unwrap();
        assert!(output.start(mixer.take_fill().unwrap()).is_ok());
        let mut pcm = [0.0_f32; 256];
        output.render(&mut pcm, timing(0));
        assert!(pcm.iter().all(|sample| *sample == 0.));
        assert!(
            playback.is_stopped(),
            "start={start:?} end={end:?} cursor={:?} output={:?}",
            playback.queue_cursor(),
            output.status()
        );
        assert!((playback.position_seconds() - 1. / 48_000.).abs() < 1e-9);
        output.stop().unwrap();
        drop(mixer);
        service.drain_and_join().unwrap();
        assert_eq!(pool.usage().bytes, 0);
    }
}

#[test]
fn playback_end_validates_quantized_bounds_and_keeps_pcm_residency() {
    use std::time::Duration;
    let pool = pool();
    let time = |frame: f64| Duration::from_secs_f64(frame / 48_000.);
    for end in [0., 0.1, 65.] {
        assert!(
            sound(&pool, vec![[0.2; 2]; 64])
                .with_playback(Duration::ZERO, Some(time(end)), None)
                .is_err()
        );
        assert_eq!(pool.usage().bytes, 0);
    }
    let clip = sound(&pool, vec![[0.2; 2]; 64]);
    let bytes = clip.resident_bytes();
    let mut clip = clip
        .with_playback(time(10.), Some(time(32.)), None)
        .unwrap();
    assert_eq!(clip.resident_bytes(), bytes);
    assert_eq!(pool.usage().tickets, 1);
    assert_eq!(clip.seek(time(3.)), time(3.));
    assert_eq!(clip.seek(time(100.)), time(32.));
    drop(clip);
    assert!(
        sound(&pool, vec![[0.2; 2]; 64])
            .with_playback(
                Duration::ZERO,
                Some(time(32.)),
                Some(AudioLoopRegion {
                    start: time(10.),
                    end: Some(time(40.))
                })
            )
            .is_err()
    );
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn group_buses_multiply_independently_and_share_one_admission_limit() {
    let mut reclaimer =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 128);
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 4,
            internal_buffer_frames: 64,
        },
        &reclaimer,
    )
    .unwrap();
    let pool = pool();
    let half = 20.0 * 0.5_f32.log10();
    mixer
        .set_group_volume_db(Some(AudioGroup::Music), half, std::time::Duration::ZERO)
        .unwrap();
    mixer
        .set_group_volume_db(Some(AudioGroup::Voice), -60.0, std::time::Duration::ZERO)
        .unwrap();
    mixer
        .set_group_volume_db(Some(AudioGroup::Ui), half * 2.0, std::time::Duration::ZERO)
        .unwrap();
    mixer
        .set_group_volume_db(None, half, std::time::Duration::ZERO)
        .unwrap();
    // Settings precede admission; every group may use the full shared capacity.
    let mut handles = Vec::new();
    for (i, group) in AudioGroup::ALL.into_iter().enumerate() {
        let value = (i + 1) as f32 * 0.1;
        handles.push(
            mixer
                .play_in(group, sound(&pool, vec![[value; 2]; 256]).looping())
                .unwrap(),
        );
    }
    let rejected = mixer
        .play_in(AudioGroup::Ui, sound(&pool, vec![[0.1; 2]; 256]))
        .err()
        .unwrap();
    assert!(rejected.reason.retryable());
    drop(rejected);
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    for _ in 0..4 {
        stereo_tail(&mut output);
    }
    near(stereo_tail(&mut output), [0.225; 2]);
    // Changes affect active voices without replacing their individual gain.
    mixer
        .set_group_volume_db(Some(AudioGroup::Music), -60.0, std::time::Duration::ZERO)
        .unwrap();
    handles[AudioGroup::Effects as usize]
        .set_volume_db(half, std::time::Duration::ZERO)
        .unwrap();
    for _ in 0..4 {
        stereo_tail(&mut output);
    }
    near(stereo_tail(&mut output), [0.125; 2]);
    mixer
        .set_group_volume_db(None, -60.0, std::time::Duration::ZERO)
        .unwrap();
    for _ in 0..4 {
        stereo_tail(&mut output);
    }
    near(stereo_tail(&mut output), [0.0; 2]);
    // All four retirement queues must be serviced even with a quota of one.
    for handle in &mut handles {
        handle.stop();
    }
    for _ in 0..4 {
        stereo_tail(&mut output);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while pool.usage().tickets != 0 {
        mixer.reclaim_unused(1);
        stereo_tail(&mut output);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    output.stop().unwrap();
    drop(mixer);
    reclaimer.drain_and_join().unwrap();
}

#[test]
fn sound_and_bus_envelopes_pan_and_suspend_follow_consumed_frames() {
    use std::time::Duration;
    let mut reclaimer =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 128);
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 4,
            internal_buffer_frames: 64,
        },
        &reclaimer,
    )
    .unwrap();
    let pool = pool();
    let mut handle = mixer
        .play_in(
            AudioGroup::Music,
            sound(&pool, vec![[0.2; 2]; 256])
                .looping()
                .with_fade_in(Duration::from_millis(100)),
        )
        .unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let first = stereo_tail(&mut output)[0];
    for _ in 0..18 {
        stereo_tail(&mut output);
    }
    let middle = stereo_tail(&mut output)[0];
    assert!(
        first >= 0.0 && middle > first && middle < 0.1,
        "{first}, {middle}"
    );
    for _ in 0..25 {
        stereo_tail(&mut output);
    }
    near(stereo_tail(&mut output), [0.2; 2]);
    handle.set_pan(1.0, Duration::from_millis(100)).unwrap();
    for _ in 0..45 {
        stereo_tail(&mut output);
    }
    let right = stereo_tail(&mut output);
    assert!(right[0].abs() < 0.0001 && right[1] > 0.2, "{right:?}");
    handle.set_pan(0.0, Duration::ZERO).unwrap();
    mixer
        .set_group_volume_db(Some(AudioGroup::Music), -60.0, Duration::from_millis(100))
        .unwrap();
    for _ in 0..18 {
        stereo_tail(&mut output);
    }
    let fading = stereo_tail(&mut output)[0];
    assert!(fading > 0.0 && fading < 0.2);
    let position = handle.position_seconds();
    output.suspend().unwrap();
    for _ in 0..60 {
        near(stereo_tail(&mut output), [0.0; 2]);
    }
    assert_eq!(handle.position_seconds(), position);
    output.resume().unwrap();
    let resumed = stereo_tail(&mut output)[0];
    assert!(resumed > 0.0 && resumed <= fading, "{fading}, {resumed}");
    for _ in 0..25 {
        stereo_tail(&mut output);
    }
    near(stereo_tail(&mut output), [0.0; 2]);
    // A fresh tween from silence must remain finite, including Rust -inf input.
    handle
        .set_volume_db(f32::NEG_INFINITY, Duration::ZERO)
        .unwrap();
    mixer
        .set_group_volume_db(Some(AudioGroup::Music), 0.0, Duration::ZERO)
        .unwrap();
    for _ in 0..4 {
        stereo_tail(&mut output);
    }
    handle
        .set_volume_db(0.0, Duration::from_millis(100))
        .unwrap();
    for _ in 0..45 {
        assert!(stereo_tail(&mut output).iter().all(|v| v.is_finite()));
    }
    near(stereo_tail(&mut output), [0.2; 2]);
    output.stop().unwrap();
    drop(mixer);
    reclaimer.drain_and_join().unwrap();
    assert_eq!(pool.usage().tickets, 0);
}
#[test]
fn real_kira_mixes_stereo_then_maps_explicit_71_and_freezes_on_suspend() {
    let service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 65,
            queued_work: 257,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let (mut output, format) = setup(ChannelLayout::Surround71, SampleFormat::F32, 7);
    assert_eq!(format.mixer_channels, 2);
    assert_eq!(format.device_channel_layout.channels().len(), 8);
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 257,
            internal_buffer_frames: 4097,
        },
        &service,
    )
    .unwrap();
    let pool = pool();
    let first = mixer
        .play(sound(&pool, vec![[0.2, -0.1]; 4800]))
        .ok()
        .unwrap();
    let second = mixer
        .play(sound(&pool, vec![[0.3, -0.2]; 4800]))
        .ok()
        .unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    assert!(mixer.take_fill().is_none());
    let mut pcm = [42.0f32; 80 * 8];
    output.render(&mut pcm, timing(10));
    // Kira static sounds have a short interpolation startup; assert steady samples.
    for frame in pcm.chunks_exact(8).skip(8) {
        assert!((frame[0] - 0.5).abs() < 1e-5, "{frame:?}");
        assert!((frame[1] + 0.3).abs() < 1e-5);
        assert_eq!(&frame[2..], &[0.0f32; 6]);
    }
    let position = first.position_seconds();
    let clock = output.clock_observation();
    assert_eq!(clock.submitted_frames, 80);
    assert_eq!(clock.estimated_presented_frames, 0);
    assert_eq!(clock.precision, ClockPrecision::Estimated);
    output.suspend().unwrap();
    pcm.fill(42.0);
    output.render(&mut pcm, timing(20));
    assert_eq!(pcm, [0.0f32; 640]);
    assert_eq!(first.position_seconds(), position);
    assert_eq!(output.clock_observation(), clock);
    output.resume().unwrap();
    output.render(&mut pcm, timing(30));
    assert!(first.position_seconds() > position);
    assert!(second.position_seconds() > position);
    assert_eq!(output.clock_observation().submitted_frames, 160);
    assert_eq!(output.clock_observation().estimated_presented_frames, 78);
    output.stop().unwrap();
    let position = first.position_seconds();
    output.render(&mut pcm, timing(40));
    assert_eq!(pcm, [0.0f32; 640]);
    assert_eq!(first.position_seconds(), position);
}
#[test]
fn conversion_zeroes_scratch_sanitizes_nonfinite_chunks_and_limits_integers() {
    let (mut output, _) = setup(ChannelLayout::Surround71, SampleFormat::I16, 2);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    assert!(
        output
            .start(Box::new(move |pcm| {
                assert_eq!(pcm.len() % 2, 0);
                assert!(!pcm.is_empty() && pcm.len() <= 4);
                assert!(pcm.iter().all(|v| *v == 0.0));
                let call = counted.fetch_add(1, Ordering::Relaxed);
                if call == 0 {
                    pcm.copy_from_slice(&[2.0, -2.0, f32::NAN, f32::INFINITY]);
                }
                // Deliberately leave later chunks untouched: prezero must prevent stale samples.
            }))
            .is_ok()
    );
    let mut out = [123i16; 5 * 8];
    output.render(&mut out, timing(1));
    assert_eq!(&out[0..8], &[i16::MAX, i16::MIN, 0, 0, 0, 0, 0, 0]);
    assert!(out[8..].iter().all(|v| *v == 0));
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    let before = calls.load(Ordering::Relaxed);
    let epoch = output.status().output_epoch;
    let mut malformed = [123i16; 15];
    output.render(&mut malformed, timing(2));
    assert_eq!(malformed, [0; 15]);
    assert_eq!(calls.load(Ordering::Relaxed), before);
    assert_eq!(output.status().state, AudioState::Failed);
    assert!(output.status().output_epoch.0 > epoch.0);
    assert_eq!(
        output.status().latest_device_error.unwrap().kind,
        AudioErrorKind::DeviceContract
    );
    assert_eq!(
        output.clock_observation().precision,
        ClockPrecision::Unavailable
    );
    assert!(output.stop().is_err());
    assert_eq!(output.status().state, AudioState::Stopped);
}
#[test]
fn mono_unsigned_silence_and_all_supported_integer_float_formats() {
    let (mut mono, _) = setup(ChannelLayout::Mono, SampleFormat::F32, 2);
    assert!(
        mono.start(Box::new(|pcm| {
            for lr in pcm.chunks_exact_mut(2) {
                lr.copy_from_slice(&[0.6, -0.2]);
            }
        }))
        .is_ok()
    );
    let mut data = [42.0f32; 3];
    mono.render(&mut data, timing(1));
    assert!(data.iter().all(|v| (*v - 0.2).abs() < 1e-6));
    let (mut unsigned, _) = setup(ChannelLayout::Surround71, SampleFormat::U16, 2);
    assert!(
        unsigned
            .start(Box::new(|pcm| {
                for lr in pcm.chunks_exact_mut(2) {
                    lr.copy_from_slice(&[4.0, -4.0]);
                }
            }))
            .is_ok()
    );
    let mut udata = [123u16; 8];
    unsigned.render(&mut udata, timing(1));
    assert_eq!(udata, [65535, 0, 32768, 32768, 32768, 32768, 32768, 32768]);
    let (mut signed, _) = setup(ChannelLayout::Stereo, SampleFormat::I32, 1);
    assert!(
        signed
            .start(Box::new(|pcm| pcm.copy_from_slice(&[3.0, -3.0])))
            .is_ok()
    );
    let mut idata = [0; 2];
    signed.render(&mut idata, timing(1));
    assert_eq!(idata, [i32::MAX, i32::MIN]);
    let (mut float, _) = setup(ChannelLayout::Stereo, SampleFormat::F64, 1);
    assert!(
        float
            .start(Box::new(|pcm| pcm.copy_from_slice(&[f32::NAN, -0.5])))
            .is_ok()
    );
    let mut fdata = [42.0f64; 2];
    float.render(&mut fdata, timing(1));
    assert_eq!(fdata, [0.0, -0.5]);
    float.render(&mut [42.0f32; 2], timing(2));
    assert_eq!(float.status().state, AudioState::Failed);
}
#[test]
fn bounded_queue_underrun_supplies_silence_counts_missing_frames_and_clock_advances() {
    let (mut output, _) = setup(ChannelLayout::Surround71, SampleFormat::F32, 8);
    let (mut producer, fill) = bounded_pcm_queue(2, output.underrun_signal()).unwrap();
    producer.push([0.2, -0.1]).unwrap();
    producer.push([0.4, -0.3]).unwrap();
    assert!(producer.push([1.0, 1.0]).is_err());
    assert!(output.start(fill).is_ok());
    let mut data = [42.0f32; 5 * 8];
    output.render(&mut data, timing(1));
    assert_eq!(&data[0..8], &[0.2, -0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(&data[8..16], &[0.4, -0.3, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert!(data[16..].iter().all(|v| *v == 0.0));
    assert_eq!(output.status().underrun_count, 1);
    assert_eq!(output.status().silent_frames, 3);
    assert_eq!(output.clock_observation().submitted_frames, 5);
    output.render(&mut data, timing(2));
    assert_eq!(output.status().silent_frames, 8);
    assert_eq!(output.clock_observation().submitted_frames, 10);
    assert_eq!(output.clock_observation().estimated_presented_frames, 3);
    output.stop().unwrap();
    let (mut producer, fill) = bounded_pcm_queue(96_001, output.underrun_signal()).unwrap();
    assert_eq!(producer.slots(), 96_001);
    producer.push([0.2, -0.1]).unwrap();
    assert!(output.start(fill).is_ok());
    output.render(&mut data, timing(3));
    assert_eq!(&data[..2], &[0.2, -0.1]);
    assert!(data[8..].iter().all(|v| *v == 0.0));
}
#[test]
fn format_change_invalidates_clock_requires_stop_and_new_kira_at_negotiated_rate() {
    let service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 8);
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    output.render(&mut [0.0f32; 16], timing(1));
    let epoch = output.status().output_epoch;
    output.change_device(AudioDeviceConfig {
        sample_rate_hz: 44_100,
        sample_format: SampleFormat::I16,
        layout: ChannelLayout::Mono,
    });
    assert_eq!(output.status().state, AudioState::Failed);
    assert!(output.status().output_epoch.0 > epoch.0);
    assert_eq!(
        output.clock_observation().precision,
        ClockPrecision::Unavailable
    );
    assert_eq!(
        output.configure(AudioRequest::default()).unwrap_err().kind,
        AudioErrorKind::Busy
    );
    assert!(output.stop().is_err());
    let format = output.configure(AudioRequest::default()).unwrap();
    assert_eq!(format.sample_rate_hz, 44_100);
    assert_eq!(format.device_sample_format, SampleFormat::I16);
    assert_eq!(format.mixer_channels, 2);
    let mut rebuilt = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    assert!(output.start(rebuilt.take_fill().unwrap()).is_ok());
    output.render(&mut [0i16; 8], timing(2));
    assert_eq!(output.clock_observation().sample_rate_hz, 44_100);
    assert!(output.status().output_epoch.0 > epoch.0);
}
#[test]
fn kira_full_play_capacity_returns_original_budgeted_request_and_stop_commands_still_work() {
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 8);
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 8,
        },
        &service,
    )
    .unwrap();
    let pool = pool();
    let mut first = mixer
        .play(sound(&pool, vec![[0.2, -0.1]; 32]))
        .ok()
        .unwrap();
    let request = sound(&pool, vec![[0.9, 0.9]; 16]);
    let bytes = request.resident_bytes();
    let rejected = mixer.play(request).err().unwrap();
    assert_eq!(rejected.reason.code, reel_core::result::RejectCode::Busy);
    assert_eq!(rejected.request.resident_bytes(), bytes);
    assert_eq!(pool.usage().tickets, 2);
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    output.render(&mut [0.0f32; 16], timing(1));
    first
        .set_volume_db(-6.0, std::time::Duration::ZERO)
        .unwrap();
    first.stop();
    output.render(&mut [0.0f32; 16], timing(2));
    assert!(first.is_stopped());
    output.render(&mut [0.0f32; 16], timing(3)); // moves unused Sound to Kira's bounded control reclamation queue.
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while service.status().completed_sounds == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(mixer.play(rejected.request).is_ok());
    assert_eq!(pool.usage().tickets, 1);
    output.stop().unwrap();
    drop(mixer);
    service.drain_and_join().unwrap();
    assert_eq!(pool.usage().tickets, 0);
}

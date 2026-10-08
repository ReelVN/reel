use reel_core::{
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
#[test]
fn real_kira_mixes_stereo_then_maps_explicit_71_and_freezes_on_suspend() {
    let (mut output, format) = setup(ChannelLayout::Surround71, SampleFormat::F32, 7);
    assert_eq!(format.mixer_channels, 2);
    assert_eq!(format.device_channel_layout.channels().len(), 8);
    let mut mixer =
        KiraMixer::new(&format, MixerLimits::default(), WakeSignal::new(|| {})).unwrap();
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
}
#[test]
fn format_change_invalidates_clock_requires_stop_and_new_kira_at_negotiated_rate() {
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 8);
    let mut mixer =
        KiraMixer::new(&format, MixerLimits::default(), WakeSignal::new(|| {})).unwrap();
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
    let mut rebuilt =
        KiraMixer::new(&format, MixerLimits::default(), WakeSignal::new(|| {})).unwrap();
    assert!(output.start(rebuilt.take_fill().unwrap()).is_ok());
    output.render(&mut [0i16; 8], timing(2));
    assert_eq!(output.clock_observation().sample_rate_hz, 44_100);
    assert!(output.status().output_epoch.0 > epoch.0);
}
#[test]
fn kira_full_play_capacity_returns_original_budgeted_request_and_stop_commands_still_work() {
    let (mut output, format) = setup(ChannelLayout::Stereo, SampleFormat::F32, 8);
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 8,
        },
        WakeSignal::new(|| {}),
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
    first.set_volume_db(-6.0).unwrap();
    first.stop();
    output.render(&mut [0.0f32; 16], timing(2));
    assert!(first.is_stopped());
    output.render(&mut [0.0f32; 16], timing(3)); // moves unused Sound to Kira's bounded control reclamation queue.
    assert_eq!(mixer.reclaim_unused(1).reclaimed_sounds, 1);
    assert!(mixer.play(rejected.request).is_ok());
    assert_eq!(pool.usage().tickets, 1);
    output.stop().unwrap();
    drop(mixer);
    assert_eq!(pool.usage().tickets, 0);
}

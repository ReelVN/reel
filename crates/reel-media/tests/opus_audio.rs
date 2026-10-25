#![cfg(feature = "decode")]

use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_media::decode::{AudioDecoder, AudioDecoderFactory, SoftwareAudioDecoderFactory};
use std::{
    io::Cursor,
    sync::{Arc, atomic::AtomicBool},
};

const MONO: &[u8] = include_bytes!("fixtures/mono.opus");
const STEREO: &[u8] = include_bytes!("fixtures/stereo.opus");
fn pool(bytes: u64) -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 8,
            max_bytes: bytes,
        },
    )
}
fn open(bytes: &'static [u8], pool: &BudgetPool) -> Box<dyn AudioDecoder> {
    SoftwareAudioDecoderFactory
        .open(
            Box::new(Cursor::new(bytes)),
            bytes.len() as u64,
            pool.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
}
fn collect(decoder: &mut dyn AudioDecoder) -> Vec<[f32; 2]> {
    let mut pcm = Vec::new();
    while decoder.decode(&AtomicBool::new(false)).unwrap() {
        pcm.extend(decoder.pcm().iter().map(|f| [f.left, f.right]));
    }
    pcm
}

#[test]
fn opus_preskip_end_trim_and_reference_pcm_share_the_audible_timeline() {
    let pool = pool(1 << 20);
    for (bytes, frames) in [(MONO, 48), (STEREO, 6576)] {
        let mut decoder = open(bytes, &pool);
        assert_eq!(decoder.info().sample_rate, 48_000);
        assert_eq!(decoder.info().frames, Some(frames));
        let pcm = collect(&mut *decoder);
        assert_eq!(pcm.len() as u64, frames);
        assert!(!decoder.decode(&AtomicBool::new(false)).unwrap());
        if bytes == MONO {
            assert!(pcm.iter().all(|f| f[0] == f[1]));
        } else {
            // Independent FFmpeg/libopus float PCM oracle; see fixtures/README.md.
            for (index, expected) in [
                (0, [0.0019890526, 0.18117233]),
                (500, [-0.18273059, -0.15810671]),
                (3000, [-0.0033762432, -0.1406271]),
                (6575, [0.31922364, 0.18201593]),
            ] {
                for channel in 0..2 {
                    assert!(
                        (pcm[index][channel] - expected[channel]).abs() < 2e-5,
                        "frame {index}, channel {channel}: {:?}",
                        pcm[index]
                    );
                }
            }
            decoder.seek(0).unwrap();
            assert!(decoder.decode(&AtomicBool::new(false)).unwrap());
            assert!(decoder.decode(&AtomicBool::new(false)).unwrap());
            let storage = decoder.pcm().as_ptr();
            while decoder.decode(&AtomicBool::new(false)).unwrap() {
                assert_eq!(
                    decoder.pcm().as_ptr(),
                    storage,
                    "PCM packet storage must be reused"
                );
            }
        }
        drop(decoder);
        assert_eq!(pool.usage().bytes, 0);
    }
}

#[test]
fn opus_seek_preroll_loop_restart_and_cancellation() {
    let pool = pool(1 << 20);
    let mut decoder = open(STEREO, &pool);
    let reference = collect(&mut *decoder);
    for target in [5000, 0, 6300, 0, 6576] {
        let actual = decoder.seek(target).unwrap();
        assert!(actual <= target.saturating_sub(3840));
        let pcm = collect(&mut *decoder);
        let skip = (target - actual) as usize;
        assert_eq!(pcm.len() - skip, reference.len() - target as usize);
        for channel in 0..2 {
            let mut dot = 0_f64;
            let mut energy = 0_f64;
            let mut expected_energy = 0_f64;
            for (actual, expected) in pcm[skip..].iter().zip(&reference[target as usize..]) {
                dot += f64::from(actual[channel]) * f64::from(expected[channel]);
                energy += f64::from(actual[channel]).powi(2);
                expected_energy += f64::from(expected[channel]).powi(2);
            }
            if expected_energy > 0. {
                let correlation = dot / (energy * expected_energy).sqrt();
                // RFC 7845 pre-roll converges the predictive codec state; a
                // restarted decode can retain a different gain on steady tones.
                assert!(
                    correlation > 0.995,
                    "Opus seek phase changed at target={target}: {correlation}"
                );
                assert!((0.8..1.2).contains(&(energy / expected_energy).sqrt()));
            }
        }
    }
    decoder.seek(0).unwrap();
    assert!(
        decoder
            .decode(&AtomicBool::new(true))
            .unwrap_err()
            .contains("cancelled")
    );
    drop(decoder);
    assert_eq!(pool.usage().bytes, 0);
    assert!(
        SoftwareAudioDecoderFactory
            .open(
                Box::new(Cursor::new(STEREO)),
                STEREO.len() as u64,
                self::pool(65536),
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
    );
}

#[cfg(feature = "encode")]
fn wav(rate: u32, frames: usize, channels: u16) -> Vec<u8> {
    let data = frames as u32 * u32::from(channels) * 2;
    let mut wav = Vec::from(&b"RIFF"[..]);
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0");
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(b"\x10\0data");
    wav.extend_from_slice(&data.to_le_bytes());
    for index in 0..frames {
        for frequency in [440., 730.].into_iter().take(usize::from(channels)) {
            let sample = ((index as f64 * frequency * std::f64::consts::TAU / f64::from(rate))
                .sin()
                * 10000.) as i16;
            wav.extend_from_slice(&sample.to_le_bytes());
        }
    }
    wav
}

#[cfg(feature = "encode")]
#[test]
fn opus_conversion_preserves_lengths_across_resampling_and_partial_packets() {
    for (rate, frames, channels) in [
        (48_000, 1, 1),
        (48_000, 960, 2),
        (44_100, 4411, 1),
        (44_100, 4411, 2),
        (96_000, 9601, 1),
        (96_000, 9601, 2),
    ] {
        let source = wav(rate, frames, channels);
        let pool = pool(4 << 20);
        let mut encoded = Vec::new();
        let info = reel_media::encode::opus(
            Cursor::new(source.clone()),
            source.len() as u64,
            &mut encoded,
            96_000,
            pool.clone(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let expected = (frames as u64 * 48_000 + u64::from(rate) / 2) / u64::from(rate);
        assert_eq!(info.frames, Some(expected));
        assert_eq!(u16::from(info.channels), channels);
        assert_eq!(pool.usage().bytes, 0);
        let mut packets = ogg::reading::PacketReader::new(Cursor::new(&encoded));
        let head = packets.read_packet().unwrap().unwrap().data;
        assert_eq!(u16::from(head[9]), channels);
        let preskip = u64::from(u16::from_le_bytes([head[10], head[11]]));
        let mut last = packets.read_packet().unwrap().unwrap();
        while let Some(packet) = packets.read_packet().unwrap() {
            last = packet;
        }
        assert!(last.last_in_stream());
        assert_eq!(last.absgp_page(), expected + preskip);
        let mut decoder = SoftwareAudioDecoderFactory
            .open(
                Box::new(Cursor::new(encoded.clone())),
                encoded.len() as u64,
                pool.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(decoder.info().frames, Some(expected));
        let pcm = collect(&mut *decoder);
        assert_eq!(pcm.len() as u64, expected, "rate={rate}, frames={frames}");
        if frames > 1000 {
            let error: f64 = pcm
                .iter()
                .enumerate()
                .skip(500)
                .map(|(index, frame)| {
                    let expected = (index as f64 * 440. * std::f64::consts::TAU / 48_000.).sin()
                        * 10000.
                        / 32768.;
                    (f64::from(frame[0]) - expected).powi(2)
                })
                .sum::<f64>()
                / (pcm.len() - 500) as f64;
            assert!(
                error.sqrt() < 0.025,
                "resampled signal error {} at {rate}",
                error.sqrt()
            );
        }
        drop(decoder);
        assert_eq!(pool.usage().bytes, 0);
        let mut repeated = Vec::new();
        reel_media::encode::opus(
            Cursor::new(source.clone()),
            source.len() as u64,
            &mut repeated,
            96_000,
            pool.clone(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(repeated, encoded);
    }
}

/// A standalone process permits OS peak RSS/private-byte measurement, including
/// opaque native codec and resampler allocations outside the PCM ledger.
#[test]
#[ignore = "explicit throughput and process-memory probe"]
fn opus_measurement() {
    let copies: u32 = std::env::var("REEL_OPUS_COPIES")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    let rounds: u32 = std::env::var("REEL_OPUS_ROUNDS")
        .unwrap_or_else(|_| "500".into())
        .parse()
        .unwrap();
    let budget = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: copies + 8,
            max_bytes: 128 << 20,
        },
    );
    let path = std::env::var_os("REEL_OPUS_INPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo.opus")
        });
    let encode = std::env::var_os("REEL_OPUS_ENCODE").is_some();
    let start = std::time::Instant::now();
    let mut frames = 0_u64;
    if encode {
        #[cfg(feature = "encode")]
        for _ in 0..rounds {
            let input = std::fs::File::open(&path).unwrap();
            let info = reel_media::encode::opus(
                input,
                std::fs::metadata(&path).unwrap().len(),
                std::io::sink(),
                96_000,
                budget.clone(),
                &AtomicBool::new(false),
            )
            .unwrap();
            frames += info.frames.unwrap();
        }
        #[cfg(not(feature = "encode"))]
        panic!("probe requires the encode feature");
    } else {
        let mut decoders: Vec<_> = (0..copies)
            .map(|_| {
                let input = std::fs::File::open(&path).unwrap();
                SoftwareAudioDecoderFactory
                    .open(
                        Box::new(input),
                        std::fs::metadata(&path).unwrap().len(),
                        budget.clone(),
                        Arc::new(AtomicBool::new(false)),
                    )
                    .unwrap()
            })
            .collect();
        for _ in 0..rounds {
            for decoder in &mut decoders {
                decoder.seek(0).unwrap();
                while decoder.decode(&AtomicBool::new(false)).unwrap() {
                    frames += decoder.pcm().len() as u64;
                }
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "{{\"encode\":{encode},\"copies\":{copies},\"rounds\":{rounds},\"frames\":{frames},\"elapsed_seconds\":{elapsed},\"audio_seconds\":{},\"ledger_peak_bytes\":{},\"released_bytes\":{}}}",
        frames as f64 / 48_000.,
        budget.usage().peak_bytes,
        budget.usage().bytes
    );
    assert_eq!(budget.usage().bytes, 0);
}

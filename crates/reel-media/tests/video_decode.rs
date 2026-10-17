#![cfg(feature = "video")]
use reel_media::video::{DecodedPacket, MovieDecoder};

#[test]
fn baseline_movie_decodes_every_picture_and_real_aac_samples() {
    let mut decoder = MovieDecoder::new(Box::new(std::io::Cursor::new(
        include_bytes!("fixtures/baseline-aac.mp4").as_slice(),
    )))
    .unwrap();
    assert_eq!((decoder.info().width, decoder.info().height), (64, 48));
    assert_eq!(decoder.info().audio_sample_rate, Some(48000));
    let mut times = Vec::new();
    let mut audio_frames = 0;
    let mut energy = 0.;
    let mut first = None;
    let mut changed = false;
    while let Some(packet) = decoder.next_packet().unwrap() {
        match packet {
            DecodedPacket::Video(frame) => {
                assert_eq!(
                    frame.planes.iter().map(Vec::len).sum::<usize>(),
                    64 * 48 * 3 / 2
                );
                assert!((frame.duration - 1. / 12.).abs() < 0.00001);
                changed |= first.as_ref().is_some_and(|v| v != &frame.planes[0]);
                first.get_or_insert_with(|| frame.planes[0].clone());
                times.push(frame.pts);
            }
            DecodedPacket::Audio(audio) => {
                assert_eq!(audio.sample_rate, 48000);
                audio_frames += audio.frames.len();
                energy += audio
                    .frames
                    .iter()
                    .map(|[a, b]| f64::from(a * a + b * b))
                    .sum::<f64>();
            }
        }
    }
    assert_eq!(times.len(), 12);
    assert!(times.windows(2).all(|t| t[1] > t[0]));
    assert!(changed);
    assert_eq!(audio_frames, 48000);
    assert!(energy > 100.);
    assert!(decoder.next_packet().unwrap().is_none());
}

#[test]
fn seek_decodes_from_idr_and_trims_audio_to_requested_sample() {
    let mut decoder = MovieDecoder::new(Box::new(std::io::Cursor::new(
        include_bytes!("fixtures/baseline-aac.mp4").as_slice(),
    )))
    .unwrap();
    decoder.seek(0.625).unwrap();
    let mut video = Vec::new();
    let mut audio = 0;
    while let Some(packet) = decoder.next_packet().unwrap() {
        match packet {
            DecodedPacket::Video(frame) => video.push(frame.pts),
            DecodedPacket::Audio(frame) => {
                assert!(frame.pts >= 0.625 - 1e-9);
                audio += frame.frames.len();
            }
        }
    }
    assert!((video[0] - 7. / 12.).abs() < 1e-9);
    assert_eq!(video.len(), 5);
    assert_eq!(audio, 18000);
}

#[test]
fn video_preserves_declared_matrix_range_and_transfer() {
    use reel_media::video::{VideoMatrix, VideoTransfer};
    let mut decoder = MovieDecoder::new(Box::new(std::io::Cursor::new(
        include_bytes!("fixtures/baseline-srgb.mp4").as_slice(),
    )))
    .unwrap();
    assert_eq!(decoder.info().audio_sample_rate, None);
    let Some(DecodedPacket::Video(frame)) = decoder.next_packet().unwrap() else {
        panic!("video expected")
    };
    assert_eq!(frame.color.matrix, VideoMatrix::Bt709);
    assert_eq!(frame.color.transfer, VideoTransfer::Srgb);
    assert!(frame.color.full_range);
}
#[test]
fn exclusive_segment_end_trims_real_aac_samples() {
    let mut decoder = MovieDecoder::new(Box::new(std::io::Cursor::new(
        include_bytes!("fixtures/baseline-aac.mp4").as_slice(),
    )))
    .unwrap();
    decoder.seek(0.2).unwrap();
    decoder.set_end(0.4);
    let mut samples = 0;
    while let Some(packet) = decoder.next_packet().unwrap() {
        if let DecodedPacket::Audio(audio) = packet {
            samples += audio.frames.len();
            assert!(audio.pts >= 0.2 - 1e-9);
        }
    }
    assert_eq!(samples, 9600);
}

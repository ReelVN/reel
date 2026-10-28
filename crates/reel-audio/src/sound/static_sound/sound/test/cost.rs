use super::*;

#[test]
#[ignore = "release-mode steady audio throughput measurement"]
fn steady_audio_cost() {
	for (sample_rate, rate) in [(48_000, 1.0), (44_100, 1.0), (48_000, 1.5)] {
		let samples: Vec<_> = (0..4096).map(|n| (n as f32 * 0.01).sin()).collect();
		let data = queue_data(sample_rate, &samples)
			.loop_region(..)
			.playback_rate(rate);
		let (mut sound, handle) = data.split();
		let info = MockInfoBuilder::new().build();
		let mut output = [Frame::ZERO; 128];
		let start = std::time::Instant::now();
		for _ in 0..40_000 {
			sound.on_start_processing();
			sound.process(&mut output, 1. / 48_000., &info);
			std::hint::black_box(&output);
		}
		eprintln!(
			"source_hz={sample_rate} rate={rate} frames={} elapsed_us={} cursor={}",
			40_000 * output.len(),
			start.elapsed().as_micros(),
			handle.position()
		);
		assert!(output.iter().any(|sample| sample.left.abs() > 0.01));
	}
}

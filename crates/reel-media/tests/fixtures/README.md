# Media fixtures

[简体中文](README.zh.md)

These original synthetic inputs are distributed under the workspace license.
FFmpeg generates the immutable files with the commands below.

`baseline-aac.mp4`: one second of a test pattern and 440 Hz sine wave.

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=1 -f lavfi -i sine=frequency=440:sample_rate=48000:duration=1 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -g 6 -c:a aac -b:a 64k -movflags +faststart -shortest baseline-aac.mp4
```

`baseline-srgb.mp4`: three silent pattern frames with BT.709 matrix, full range
and sRGB transfer in the H.264 VUI.

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=0.25 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -bsf:v h264_metadata=video_full_range_flag=1:colour_primaries=1:transfer_characteristics=13:matrix_coefficients=1 -movflags +faststart baseline-srgb.mp4
```

`mono.opus` contains 48 mono source frames; `stereo.opus` contains 6576 stereo
frames with distinct 440/730 Hz signals. Both have 312 pre-skip frames and a
partial final packet. The short mono fixture verifies that leading and trailing
trims can share a packet. The stereo PCM assertions use samples from an
independent FFmpeg float decode of this immutable file.

```sh
ffmpeg -f lavfi -i "sine=frequency=660:sample_rate=48000:duration=0.001" -c:a libopus -b:a 48000 -map_metadata -1 -fflags +bitexact -flags:a +bitexact mono.opus
ffmpeg -f lavfi -i "aevalsrc=0.35*sin(2*PI*440*t)|0.2*cos(2*PI*730*t):s=48000:d=0.137" -c:a libopus -b:a 96000 -map_metadata -1 -fflags +bitexact -flags:a +bitexact stereo.opus
ffmpeg -i stereo.opus -c:a pcm_f32le -f f32le stereo.f32le
```

Focused checks and the explicit throughput/process-memory probe:

```sh
cargo test --locked --offline -p reel-media --no-default-features --features encode opus
cargo test --locked --offline -p reel-media --no-default-features --features encode --test opus_audio opus_measurement -- --ignored --exact --nocapture
```

`REEL_OPUS_INPUT`, `REEL_OPUS_COPIES` and `REEL_OPUS_ROUNDS` select the probe's
source, simultaneously retained decoders and repeat count. `REEL_OPUS_ENCODE=1`
instead converts the source to a sink. Its ledger peak covers owned PCM/packet
storage; measure this test process's OS peak RSS/private bytes separately to
include libopus and Rubato internal allocations. Store generated sources and
measurement logs under ignored `target/reel-artifacts/`.

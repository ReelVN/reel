# Media fixtures

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

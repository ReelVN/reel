`baseline-aac.mp4` is a one-second generated test pattern and 440 Hz sine wave,
created for Reel. It contains no third-party footage or music. The fixture is
available under the repository's license. Generate it with:

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=1 -f lavfi -i sine=frequency=440:sample_rate=48000:duration=1 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -g 6 -c:a aac -b:a 64k -movflags +faststart -shortest baseline-aac.mp4
```

FFmpeg is used only to generate this immutable test input, not by Reel's player.

`baseline-srgb.mp4` uses the same generated pattern for three silent frames,
with explicit BT.709 matrix, full range and sRGB transfer in the H.264 VUI:

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=0.25 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -bsf:v h264_metadata=video_full_range_flag=1:colour_primaries=1:transfer_characteristics=13:matrix_coefficients=1 -movflags +faststart baseline-srgb.mp4
```

# 媒体测试夹具

[English](README.md)

这些原创合成输入按工作区许可证分发。以下命令使用 FFmpeg 生成固定的用例文件。

`baseline-aac.mp4` 包含一秒测试图案和 440 Hz 正弦波。

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=1 -f lavfi -i sine=frequency=440:sample_rate=48000:duration=1 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -g 6 -c:a aac -b:a 64k -movflags +faststart -shortest baseline-aac.mp4
```

`baseline-srgb.mp4` 包含三帧无声测试图案。H.264 VUI 指定 BT.709 矩阵、全范围和 sRGB 传递函数。

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12:duration=0.25 -c:v libopenh264 -profile:v constrained_baseline -pix_fmt yuv420p -bsf:v h264_metadata=video_full_range_flag=1:colour_primaries=1:transfer_characteristics=13:matrix_coefficients=1 -movflags +faststart baseline-srgb.mp4
```

`mono.opus` 包含 48 帧单声道源数据，`stereo.opus` 包含 6576 帧立体声源数据，两个声道分别为 440 Hz 和 730 Hz 信号。两份文件的预跳过量都是 312 帧，末尾数据包都只包含部分有效数据。短单声道用例验证了首尾裁剪可以分摊在同一个包上。立体声 PCM 断言使用 FFmpeg 对该固定文件独立解码得到的浮点样本。

```sh
ffmpeg -f lavfi -i "sine=frequency=660:sample_rate=48000:duration=0.001" -c:a libopus -b:a 48000 -map_metadata -1 -fflags +bitexact -flags:a +bitexact mono.opus
ffmpeg -f lavfi -i "aevalsrc=0.35*sin(2*PI*440*t)|0.2*cos(2*PI*730*t):s=48000:d=0.137" -c:a libopus -b:a 96000 -map_metadata -1 -fflags +bitexact -flags:a +bitexact stereo.opus
ffmpeg -i stereo.opus -c:a pcm_f32le -f f32le stereo.f32le
```

运行以下命令执行专项检查，或测量吞吐量和进程内存。

```sh
cargo test --locked --offline -p reel-media --no-default-features --features encode opus
cargo test --locked --offline -p reel-media --no-default-features --features encode --test opus_audio opus_measurement -- --ignored --exact --nocapture
```

`REEL_OPUS_INPUT`、`REEL_OPUS_COPIES` 和 `REEL_OPUS_ROUNDS` 分别指定输入、同时保留的解码器数量和重复次数。设为 `REEL_OPUS_ENCODE=1` 会改为编码输入并将输出写入 sink。记账峰值只涉及自有的 PCM 和数据包存储。要计入 libopus 和 Rubato 的内部分配，需单独测量进程的峰值 RSS 或私有字节数。生成的输入和测量日志放在已忽略的 `target/reel-artifacts/` 下。

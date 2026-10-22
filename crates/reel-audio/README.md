# Reel audio

`reel-audio` is the host-driven mixer for sounds, tracks, effects, clocks and
sample processing. [`reel-media`](../reel-media/README.md) supplies native output,
callback lifecycle and background PCM retirement.

Default features select the manually driven `MockBackend` and owned PCM.
Production hosts implement [`Backend`](src/backend.rs) and receive an owned
Renderer. Audio processing uses preallocated storage; control transfers retired
sounds to the media service before physical destruction.

Enable the required Symphonia container/codec features for encoded files
(`wav`, `pcm`, `mp3`, `ogg`, `vorbis`, `flac` and other declared formats).
`serde` enables supported value/settings serialization. Public source APIs
document construction, transport, queues and decoding.

```sh
cargo test --locked -p reel-audio --lib --no-default-features
cargo test --locked -p reel-media --no-default-features
cargo run --locked -p reel-dev -- audio-probe
```

The probes exercise offline mixing and worker release. Native output validation
uses the media adapter on the target device. [PROVENANCE.md](PROVENANCE.md)
records borrowed sources, licenses and local changes.

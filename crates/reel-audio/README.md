# reel-audio

Reel's host-driven audio mixer and sound renderer. The crate owns sound playback,
tracks, effects, clocks and sample processing. `reel-media` supplies the device
backend, callback lifecycle and background PCM retirement.

The default build enables no file decoders and opens no native device.
`DefaultBackend` is the manually driven `MockBackend`; production hosts implement
`backend::Backend` and receive the owned `Renderer`. Audio callbacks process
preallocated storage, while control code transfers retired sounds without
running their large destructors.

```rust
use reel_audio::{
    AudioManager, AudioManagerSettings, DefaultBackend, Frame,
    sound::static_sound::{StaticSoundData, StaticSoundSettings},
};

let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
let sound = StaticSoundData {
    sample_rate: 48_000,
    frames: vec![Frame::ZERO; 48].into(),
    settings: StaticSoundSettings::default(),
    slice: None,
};
let _playback = manager.play(sound)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

File loading and streaming through Symphonia are optional. Enable the required
container/codec features (`wav`, `pcm`, `mp3`, `ogg`, `vorbis`, `flac`, and the
other declared format features) for the input being decoded. File-loading API
examples require `symphonia`; the default examples and library use owned PCM.
The `serde` feature enables serialization for supported value/settings types.

From the repository root:

```sh
cargo test --locked -p reel-audio --lib --no-default-features
cargo test --locked -p reel-media --no-default-features
cargo run --locked -p reel-dev -- audio-probe
```

These checks cover mixing, ownership and offline callback behavior. They do not
establish physical device timing. Source origin and retained licenses are listed
in [PROVENANCE.md](PROVENANCE.md).

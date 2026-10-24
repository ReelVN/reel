# Media references

ATRAC9 behavior was checked against `krkr-rs` commit
`1416fab27035f3a934fb5349ff86942ed0cac7fe`, files
`crates/krkr-audio/src/at9.rs` and `crates/host-vita/src/audio_at9.rs`.
Reel maintains its RIFF reader, audible-range policy and SDK ownership adapter.
Metadata tests construct their RIFF bytes directly and contain no encoded audio.

Vita SDK declarations come from the pinned `vitasdk-sys` 0.3.3 dependency,
licensed MIT OR Apache-2.0. Native SDK libraries remain toolchain dependencies.

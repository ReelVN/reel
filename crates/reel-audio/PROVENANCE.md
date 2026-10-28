# Reel audio provenance

The implementation came from Kira 0.12.5, imported from crates.io on 2026-10-08.
Source: [tesselode/kira](https://github.com/tesselode/kira), revision
`74b3ef0fd8bd15f90e3b469208ee741f04374a62`, directory `crates/kira`.
The original MIT/Apache-2.0 licenses and source copyright notices remain.

Reel maintains the code as the `reel-audio` workspace package. Local changes
include bounded ownership transfer of retired sounds, decoder reservation/
cancellation checkpoints, pending seek publication, interpolation-preserving
PCM queues and worker-backed frame windows. Native device output and physical
PCM retirement are owned by `reel-media`.

Static transport rate ramps freeze while paused or held. Coalesced rate commands
carry acknowledgement identities; the existing coherent cursor publication also
contains the current rate, fixed target and remaining ramp for restoration. These
add fixed-size command/atomic state and reuse the existing resampler and parameter
interpolation, with no callback allocation or extra PCM storage.

Mixing reuses constant gain and pan coefficients within each processing block.
Changing parameters retain their original per-sample interpolation, and pan
application preserves the original multiplication order.
Resampling reads the current PCM sample directly at integral positions and
converts the fractional cursor only when a queued source changes sample rate.

The host reader contract is codec-independent. Shared source timelines, decoder
factories and the static/window preparation policy are maintained in `reel-media`;
the Symphonia packet decoder keeps its reservation and cancellation checkpoints.
Codec construction can be supplied by the media adapter, and Opus seeks include
the RFC 7845 80 ms pre-roll before publishing audible PCM.
Packet conversion reuses worker PCM capacity, with growth and old/new storage
overlap reserved before allocation.

`tests/fixtures/short-tone.mp3` is an original synthetic 660 Hz tone under the
crate license. Its priming/padding packets exercise zero-frame decode and EOF.
Reproduce it with FFmpeg 9.0/libmp3lame:

```sh
ffmpeg -f lavfi -i "sine=frequency=660:sample_rate=22050:duration=0.08" -ac 2 -c:a libmp3lame -b:a 64k -map_metadata -1 short-tone.mp3
```

Decoder coverage: `cargo test --locked --offline -p reel-audio --features mp3 mp3_ --lib`.

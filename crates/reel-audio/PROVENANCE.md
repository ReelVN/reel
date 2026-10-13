# reel-audio provenance

The audio implementation originated from Kira 0.12.5, imported from crates.io
on 2026-10-08. Source repository: https://github.com/tesselode/kira;
revision `74b3ef0fd8bd15f90e3b469208ee741f04374a62`, directory `crates/kira`.
The MIT and Apache-2.0 license files are retained from that revision's repository
root, together with source copyright notices.

Reel maintains this code as the workspace package `reel-audio`. It follows the
workspace version, Rust edition and safe-code policy. There is no upstream
package alias, patch, source-hash gate or synchronization requirement.

The sound arena checks retirement queue capacity before releasing a slot.
Main-track control exposes bounded, pure ownership transfer of retired sounds;
playing a new sound does not silently destroy old PCM. Deterministic scheduling
checks remain in `src/backend/resources/reel_tests.rs`. Device output, callback
lifecycle and deferred PCM destruction belong to `reel-media`; the duplicate
CPAL backend was removed from this library. Optional decoding and serialization
features remain opt-in, with ordinary dependencies pinned in the workspace lock.

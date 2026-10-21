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
Main-track and sub-track control expose bounded, pure ownership transfer of retired sounds;
playing a new sound does not silently destroy old PCM. Deterministic scheduling
checks remain in `src/backend/resources/reel_tests.rs`. Device output, callback
lifecycle and deferred PCM destruction belong to `reel-media`; the duplicate
CPAL backend was removed from this library. Optional decoding and serialization
features remain opt-in, with ordinary dependencies pinned in the workspace lock.

Static decoding exposes a PCM reservation/cancellation checkpoint, used by the
media adapter before buffer growth and final Arc transfer. The same decoder
continues to serve file and host-provided sources.
Packet decoding counts the old PCM allocation twice only when it actually
grows; packets that fit reuse the existing allocation and conversion scratch.

Static sound seeks publish their pending position until the output confirms that
request, discard the old interpolation history and keep pause/terminal state.
Absolute loop seeks preserve the intro and wrap in constant time at the loop end.

Static PCM queues use bounded incoming/retired rings and preserve the interpolation
cursor across source boundaries, including sample-rate changes. A finite startup
fence prepares same-turn segments before consumption. Up to three source segments
are retained for interpolation lookahead so seek still targets the current source;
ownership returns through the media adapter's existing background reclaimer.
Announcements after the terminal tail return `QueueClosed`. A full incoming ring
returns the original PCM and residency owner directly, so rejection requires no
additional allocation or ownership transfer to the audio callback.

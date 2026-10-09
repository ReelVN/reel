# Kira 0.12.5 Reel fork

Imported from the unmodified crates.io package kira 0.12.5 on 2026-10-08.
Upstream: https://github.com/tesselode/kira
Registry crate SHA-256: `aae7115ef1161313b8f71f49487702a662ab333bd2a47bf69af410b69112a519`.
VCS commit: `74b3ef0fd8bd15f90e3b469208ee741f04374a62`, path `crates/kira`.
License: MIT OR Apache-2.0; the published crate omitted license files, so `LICENSE-MIT` and
`LICENSE-APACHE` were fetched from that exact VCS commit's repository root:
[MIT](https://raw.githubusercontent.com/tesselode/kira/74b3ef0fd8bd15f90e3b469208ee741f04374a62/LICENSE-MIT),
[Apache](https://raw.githubusercontent.com/tesselode/kira/74b3ef0fd8bd15f90e3b469208ee741f04374a62/LICENSE-APACHE).
`LICENSE-SOURCES.json` records their hashes and URLs.
`UPSTREAM-SHA256.json` records original imported file hashes; registry .cargo-ok
and its standalone Cargo.lock are omitted. Cargo.toml/Cargo.toml.orig and the
upstream version are retained. Only this crate is imported; atomic-arena 0.1.2
and the rest of the ecosystem remain ordinary Cargo.lock dependencies.

Production changes are restricted to two upstream files, recorded in
`REEL-PATCH.patch`: backend/resources.rs guards unused capacity BEFORE
DrainFilter.next frees a slot; track/main/handle.rs exposes quota-limited
control reclamation plus pure-move try_pop_unused/try_take_unused_sound. Reel
reserves an existing rtrb destination chunk before taking Sound and uses host-owned
background destruction. Main-track play bypasses implicit reclamation so it cannot
silently destroy a batch beyond a control tick's quota. Other upstream control
APIs retain their original implicit cleanup behavior and are disabled by Reel's
zero capacities. This fork is used by Reel's private main-track Backend bridge;
it is not a replacement API for those unsupported engine features.

On a full retirement queue, finished Sound stays in its arena slot and applies
backpressure. No resource is dropped, no PCM is destroyed, and no control thread
is waited on by the RT retirement path. Reclaim returns a count and a conservative
maintenance flag; hitting a quota always requests a later maintenance turn.
A false flag only observed no currently published resources, not that active or
in-flight PCM residency ended. Reclaim is control-only and each resource drop is
indivisible; Reel measures maximum supported PCM destruction separately.

For Reel's configured sound capacity N (1..=256), active/pending resources share
N arena reservations; retired resources have another N bounded queue slots.
Each reclaim performs at most min(limit, N) destructors; main-track play performs
zero old-resource destructors. Reaching even a zero quota returns the conservative
maintenance flag, never a claim that the queue was drained.

`src/backend/resources/reel_tests.rs` and its call/module declarations are
`#[cfg(test)]` only, not a Cargo feature. Scheduling barriers cannot enter normal
library builds. The regression uses actual StaticSound/Renderer PCM and pauses
only a legal arena-free -> unused-publish interval.

The exact VCS commit's repository-root rustfmt.toml was omitted by publishing.
It is restored unchanged (hard_tabs = true), with URL/hash in
UPSTREAM-CONFIG-SOURCES.json; no unmodified imported source is reformatted.

From the repository root, `cargo run -p reel-dev --locked -- audit-kira` checks
all 126 original hashes after reversing the approved patch, exact-VCS
licenses/config and the Rust source closure. An optional official package
directory adds independent original-byte comparisons. `audio-regression` (also
with `--release`) runs the unchanged real-Kira scheduling/quota/play test in an
isolated copy using the curated media test lock. Generated logs/copies remain
under ignored `target/reel-artifacts/`; see [Rust tooling](../../tools/reel-dev/README.md).

# D01 shared contracts

This crate implements the foundation subset of [Issue #1](https://github.com/jhq223/reel/issues/1), against design 1.1 at `5a7cfd4c0802e604928e72f623ffec024d864d71`. Scope is the root Cargo/toolchain configuration, `crates/reel-core/**`, and these development notes. Only the existing core crate is added to the workspace; `crates/*` lets independently reviewed crates join later.

Specification locations: [engine architecture](../../docs/engine-architecture.md), section three's trait signatures and “基础类型与结果约定”, “资源与工作执行”, and “音频时钟与媒体读取”; [Aside specification](../../docs/aside-language.md), sections twelve, sixteen, and eighteen. The Rust service traits follow the specified signatures. The crate has no ordinary or development dependencies; a needed future dependency must be pinned directly rather than copied in.

## Public ownership

- `identity.rs`: independent fixed-width IDs, the three-field session scope, process request identity, and owned asset names with an opaque 32-byte content version. Version lookup never falls back to a newer release. Assigning fresh process/session/owner identities remains the owning service's responsibility.
- `request_ids.rs`: a typed, shareable service counter. Share it across replacement adapters in the same request domain. Admission is serialized; rejection returns the original request without consuming an ID. The final `u64` value can be accepted once; exhaustion returns `ServiceFault` without wrapping. The admission callback must not reenter that ID stream.
- `budget.rs`: `BudgetPool` clones share an account. `BudgetTicket` cannot be cloned, and transfers with `OwnedBytes`. Entire `Vec` capacity is charged. Output retained by a consumer and saved data retained by a store keep their own tickets; draining an event does not release those bytes. Accounting uses a mutex, not `AtomicU64`, and is a control-side facility.
- `result.rs`: owning rejection reasons and `Completed / Cancelled / Failed`. Only `Busy` and `Unavailable` are retryable. An accepted event carries its ID, original scope, and completion ticket. A save event also retains the exact process request identity, including on cancellation or failure.
- `io.rs`, `work.rs`, `clock.rs`: service traits and owned descriptions, bounded ranges, explicit IO storage capabilities, separate work input/output/temporary/completion reservations, monotonic nanoseconds and optional UTC milliseconds. `SaveWrite` is process-owned by construction. There is no universal `Send + Sync` constraint on these traits.
- `wake.rs`: clones coalesce platform notifications. The loop acknowledges before draining sources, and rechecks `is_pending` before sleeping. The injected callback must be short and nonblocking; this general callback is not certified for realtime audio.

The `test-support` feature exposes executable fixtures, excluded from the normal core build:

- `MemoryAssetIo` has independent read/describe and save queues. `complete_next_read` and `complete_next_save` explicitly drive memory operations; `poll` only appends completed events. Exact resource versions are immutable. Reads can reach EOF or fail for an unavailable version. Same-slot saves commit in submission order, reject non-increasing sequence numbers, and move the entire data buffer into the store only on success. Injected failure and cancellation preserve the previous slot. The fixture advertises only `Visible`; requesting `Durable` is rejected before acceptance. It makes no filesystem, crash-recovery, or durable-ledger claim.
- `CooperativeExecutor` registers only fixture copy encoding and XOR-packet verification. These are cursor-based operations using at most 256 source bytes per work unit, preallocated output and scratch storage, and a clock check before each unit. `WorkProgress.bytes` measures source bytes handled, separately from resident-byte reservations. A packet with nonzero aggregate XOR actually fails validation. This is not a production codec or integrity algorithm. Visible work precedes normal work, which precedes prefetch; equal priority jobs round-robin across slices.
- The shared fixture queue preallocates a terminal slot before publishing acceptance. A non-cloneable completion slot is consumed on settlement. Pending and undrained terminal records share the request-count limit; charged bytes conservatively include input, maximum output, scratch, completion and request metadata until drain. Cancellation of pending work settles once and releases its data; later driver calls cannot produce a business result. Once completion has been enqueued, it has already won settlement, and later cancellation is a no-op. The business layer first revokes its wait/subscription, then calls service cancellation; engine routing checks both wait activity and Scope before resuming business. An already settled result remains for resources and the process ledger, preserving an actual save commit. This distinction was confirmed by the integration owner during review. `poll`/`drain` append at most the supplied limit, including across IO lanes; callers preallocate the output vector.

Fixture constructors accept explicit `QueueLimits`; their convenience `new` creates an isolated ID domain. Use `with_ids` for replacement/shared-service checks. Tight limits in the contract scenarios are test settings, not platform defaults. Target adapter tasks choose actual queue capacities and independently verified storage levels.

## Behavioral evidence and commands

Run from the repository root with Rust 1.90.0:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test -p reel-core --all-features --locked
cargo test -p reel-core --all-features --release --locked
cargo check -p reel-core --no-default-features --locked
cargo check -p reel-core --all-targets --all-features --locked --target i686-unknown-linux-gnu
cargo tree -p reel-core --all-features --locked
cargo tree -p reel-core --no-default-features --locked
cargo run -p reel-core --release --features test-support --example cooperative_probe --locked
git diff --check
```

All commands above passed on 2026-10-08. In this environment Rust initially was absent from PATH; pinned rustup 1.28.2 installed toolchain 1.90.0 outside the repository. Shell setup here was:

```sh
export PATH=/workspace/reel-cargo/bin:$PATH
export CARGO_HOME=/workspace/reel-cargo
export RUSTUP_HOME=/workspace/reel-rustup
export CARGO_TARGET_DIR=/workspace/reel-target
```

`tests/contracts.rs` exercises original buffer/key-pointer preservation on rejection and retry, byte and request exhaustion, undrained terminal backpressure, repeated cancellation, output and completion-ticket residency, fixed versions/EOF/errors, stale epochs released without business delivery, process-owned saves under a saturated read lane, commit order, failure preserving the old slot, and rejection of unprovided durability. Work scenarios check partial progress, byte/unit/deadline limits using a fake clock, priority selection, cancellation after partial processing, real validation failure, empty work, and missing output/scratch reservations. Shared IO counters are exercised across adapter replacement and both IO lanes at the end of the `u64` range. Ticket ownership is also moved across a thread; wake coalescing is driven from worker threads.

The first run caught a test expectation error: it counted 4 bytes reserved by a missing-version read after that failure had already returned its output ticket. The observed residency was the surviving old-version read's 99-byte reservation. The corrected scenario checks that those 99 bytes remain charged through delivery and are returned when that stale event is dropped. Both debug and release behavior runs then passed.

Both dependency-tree commands report just:

```text
reel-core v0.1.0
```

No window, GPU, audio, VM, compiler, watcher, or copied dependency is linked.

The release probe actually copies and verifies 1 MiB, with four units / 1024 source bytes allowed per advance. On this Linux x86_64 KVM environment, AMD EPYC 9V74, rustc 1.90.0 / LLVM 20.1.8, the final recorded run observed:

```text
submit_ns=5608
bytes=1048576 slices=1024 p50_ns=340 p99_ns=4036 max_ns=52499
peaks: input=1048576 output=1048576 temporary=256 completion=512 queue=2098188
after_drain: output=1048576 completion=512 queue=0
after_output_drop: all tickets and resident bytes returned
```

These observations include the final slice's terminal bookkeeping and input release. The probe separately times admission/preallocation and verifies every returned byte. They describe this fixture in a virtualized environment; they do not establish codec, device, low-end hardware, or frame-rate performance. Pool/queue peaks are logical reserved-byte counters, not RSS measurements.

## Remaining target work and author guide advice

The i686 result is a 32-bit compilation check, including fixtures, tests, and example; no i686 binary or device behavior was run. Windows, macOS, mobile, Web and PS Vita builds/behavior, platform durable storage/fault injection, blocking-worker cancellation, and production codecs remain unrun and outside this issue's implementation scope. Session routing, snapshot restoration and persistent operation ledgers belong to the engine, not these fixtures. No interface conflict requiring a design deviation was encountered.

Do not mark the author-facing APIs implemented yet. [Chinese guide](../../docs/guide/zh-CN.md), section eleven “存档、历史、路线与快进”, and [English guide](../../docs/guide/en.md), section eleven “Saves, History, Routes, and Skipping”, still correctly label the service signatures as awaiting implementation. D01 provides lower-level types/fixtures, not `saves.save`, `await SaveRequest`, or `persistent.flush`. No bilingual snippet or status edit is needed for this PR; those sections should gain implementation status and runnable examples only with the corresponding engine/VM work, maintained by the project lead.

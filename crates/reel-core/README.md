# Reel shared contracts

`reel-core` defines the identities, owned bytes, budgets and service results
shared by the engine and platform adapters. The
[architecture](../../docs/engine-architecture.md) defines their composition.

| Source | Responsibility |
| --- | --- |
| [identity](src/identity.rs) | Session/owner/request identity, asset keys and exact content versions |
| [request IDs](src/request_ids.rs) | Shared admission counters across replacement adapters |
| [budget](src/budget.rs) | Shared pools and unique transferable reservations |
| [result](src/result.rs) | Owned rejection, completion, cancellation and failure |
| [IO](src/io.rs), [work](src/work.rs) | Bounded requests, storage capabilities and explicit work reservations |
| [clock](src/clock.rs), [wake](src/wake.rs) | Monotonic time and coalesced host notification |

`OwnedBytes` retains a reservation for the entire vector capacity until physical
release. Consumer-held output and committed saves keep their tickets.
`BudgetTicket::resize` reserves growth atomically before allocation. Rejection
preserves the original request and its reservations. Accepted terminal records
keep capacity until drained; the host revokes business waits before cancellation
and validates session/owner identity at delivery.

Acknowledge a wake before draining its sources and recheck pending work before
sleeping. The generic wake callback executes short control-side work. Real-time
audio uses its adapter's dedicated notification path.

The `test-support` feature provides `MemoryAssetIo` with independent read/save
queues and `CooperativeExecutor` with sliced copy/XOR fixtures. Their explicit
limits and injected completion drive service ownership checks.

```sh
cargo test --locked -p reel-core --all-features
cargo check --locked -p reel-core --no-default-features
cargo run --locked -p reel-core --release --features test-support --example cooperative_probe
bash crates/reel-core/tests/support/run_dealloc_probe.sh
```

The deallocation probe observes input/output/scratch release while the owning
reservation remains charged. Generated measurements belong under ignored
`target/reel-artifacts/`.

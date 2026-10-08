# L04 compiler validation and measurements

Implementation measured: `2edce46ca236c6424db61d8f3dd89b0e5fd19b25`.
Rust 1.96.0, Linux x86_64/glibc 2.41, 2026-10-08, release optimization.
Each accepted workload ran seven fresh processes; times below are medians.
These are local compiler measurements, with no earlier lowering baseline or
runtime throughput claim. Container scheduling can affect elapsed times.

## Recorded results

`lower` builds IR and shared tables; `emit + verify` relocates code and runs B01
verification. Parsing, L03 analysis and bytecode codec times are separately
recorded in the raw data. Work counts are prepaid compiler operations; B01
verification work has its own counter. Retained bytes cover new Vec/String
capacities plus verifier-derived metadata after scratch has been released.
Every one of the 91 accepted samples asserts equality with the actual retained
allocation from `stats_alloc`. Borrowed analyzed input is excluded. Peak bytes
are a conservative capacity upper bound including replacement allocations and
verification scratch; they are not measured RSS or an allocator peak.

| Workload | Size | Lower ms | Emit + verify ms | Retained bytes | Peak upper bytes | Compiler work | Instructions | Max registers/function |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Plain functions | 1,000 | 1.056 | 0.766 | 710,800 | 919,476 | 529,784 | 5,002 | 1 |
| Plain functions | 10,000 | 12.649 | 8.100 | 9,632,720 | 13,023,224 | 6,667,875 | 50,002 | 1 |
| Mutable captured closures | 1,000 | 3.371 | 2.799 | 2,211,962 | 2,409,022 | 2,003,597 | 28,002 | 4 |
| Mutable captured closures | 2,000 | 6.592 | 5.779 | 4,424,098 | 4,817,766 | 4,138,284 | 56,002 | 4 |
| Reversed named arguments | 1,000 | 0.760 | 0.502 | 280,396 | 379,152 | 471,623 | 4,014 | 1,002 |
| Reversed named arguments | 10,000 | 10.795 | 6.728 | 4,468,876 | 6,042,192 | 6,553,806 | 40,014 | 10,002 |
| Sequential temporary expressions | 1,000 | 0.324 | 0.454 | 333,097 | 529,965 | 88,539 | 6,007 | 3 |
| Sequential temporary expressions | 10,000 | 3.031 | 4.373 | 3,212,585 | 4,785,709 | 863,795 | 60,007 | 3 |

The 10,000-expression function reuses three temporary registers. Named calls
retain the complete contiguous actual-argument window until the Call; their
register count grows with simultaneously live arguments, as required. Plain and
capture workloads have separate functions, so aggregate register totals grow
while their per-function maximum stays bounded.

Three larger workloads were rejected normally, rather than reported successful:

| Workload | Size | Stage | Diagnostic | Source byte range | Compiler work charged |
| --- | ---: | --- | --- | --- | ---: |
| Mutable captured closures | 5,000 | Shared verification | B01 capacity/record limit | 0..253890 | 10,778,201 |
| Sequential temporary expressions | 25,000 | Shared verification | B01 capacity/record limit | 0..100009 | 2,486,299 |
| Sequential temporary expressions | 50,000 | IR construction | Compiler instruction limit | 133335..133338 | 2,649,064 |

Default compiler and B01 quotas are independent. The first two exceed B01's
shared record admission; the third reaches the compiler's 200,000-instruction
budget before constructing further operations. Located errors are returned and
no bytecode is executed.

## Reproduction and correctness checks

Run from the repository root with the pinned toolchain on PATH:

```sh
cargo build -p aside-compiler --example lowering_probe --release --locked
python3 crates/aside-compiler/bench-results/lowering/run_probe.py
```

The runner preserves individual samples, medians, environment/revision and
rejections in [bench-results/lowering](bench-results/lowering/). The source
generator and retained-allocation assertion are in
[examples/lowering_probe.rs](examples/lowering_probe.rs).

Final implementation checks passed in debug and release:

```sh
cargo fmt --all --check
cargo clippy -p aside-compiler --all-targets --locked -- -D warnings
cargo test -p aside-compiler --all-targets --locked
cargo test -p aside-compiler --all-targets --release --locked
python3 crates/aside-compiler/tests/fixtures/quirrel/check_integrity.py
git diff --check
```

The compiler suites contain 97 tests, including 19 lowering contracts. Tests
compile source with L03, inspect shared instruction register reads/writes and
control-flow liveness, check callee/argument source order and saved operands
across nested calls, distinguish uninitialized bindings from null, compare
capture/parameter identities, preserve source Context and trusted native
registration/namespace provenance, and verify/encode/decode/disassemble with
B01. Every emitted PC maps to a valid original UTF-8 byte range. Regression
coverage includes value subexpressions whose branches return and the remaining
short-circuit fallthrough edge. Quota tests cover work, capacity, records,
instructions, registers, blocks, depth, source admission and shared emission.
Fixture integrity reports 10 immutable T01 sources/maps and 32 unchanged
assertion mappings; execution remains `waiting_runtime`.

## Pending acceptance

No VM dispatch or native host invocation was run. V03 independent execution
validation remains pending its platform blocker; this task does not reroute it.
Default expressions containing an explicit return to the enclosing body need
an agreed shared transfer contract, demonstrated by located diagnostic test
`missing_default_transfer_and_shared_emission_limits_are_located`. B01 also has
no single Context encoding for combined derived+init declarations; the source
flags are retained but such a function is diagnosed. Full-language deferred
scopes are listed in [LOWERING.md](LOWERING.md). Integration-owner review and
dependency acceptance remain separate from these compiler results.

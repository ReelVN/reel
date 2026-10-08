# Native root initialization comparison

Measured after the complete §23 fault implementation (`2842fb4`), rather than
comparing the older, incomplete fault semantics against the new implementation.
Probe instrumentation baseline: `73467e3`; optimized implementation: `f23014d`.
Both use V02 `cceb151` and B01 `be4f98e`, identical default limits and fixtures.
2026-10-08T20:00Z, Linux x86_64, AMD EPYC 9V74, 5 allocated vCPU; Rust 1.96.0.
The development host is not isolated or pinned. These are host timings, not a
device guarantee. [All fourteen raw outputs](NATIVE-PERF-SAMPLES.jsonl) are kept.

## Cause and change

The former NativePhase::Copy wrote NULL to every root element after copying
arguments, solely to satisfy a whole-window initialized borrow. V02 open had
already physically filled the window with NULL and cleared its bitmap before
publishing its descriptor. Original default width was 515; §23's eight bounded
fault slots made the measured width 523. Ten thousand calls repeated 5,230,000
barrier/write/bitmap operations, each with an internal dispatch step.

Host continuation/fault views now validate just their logical initialized span.
Entry-to-bind omits the redundant NULL loop. Open still clears the entire range
under the original conservative native construction charge. Real writes keep
the insertion barrier and initialized bit. Ordinary uninitialized reads fail,
including holes surrounding the requested span; stale generations still fail.
No GC scanning, physical root capacity or free-span release work is discounted.

## Seven interleaved pairs

Each process executes a million-iteration numeric loop, then 10,000 calls
alternating Ready/Pending, a full incremental GC with one native live, and owner
cancellation. Odd pairs run baseline then optimized; even pairs reverse order.
The call input/result, adapter, budgets, pool sizes and allocator probe match.
Each row is a median of seven runs; per-call p50/p95 are medians of the seven
within-run percentiles, not a pooled percentile.

| Measurement | Baseline | Optimized |
| --- | ---: | ---: |
| 10,000 calls total, µs | 268,864 | 44,515 |
| Call p50, ns | 21,873 | 3,836 |
| Call p95, ns | 41,603 | 4,056 |
| Call total min–max, µs | 240,886–290,705 | 39,801–54,190 |
| Numeric loop total, µs | 355,577 | 366,767 |
| Numeric total min–max, µs | 340,725–423,690 | 301,781–466,545 |
| Redundant NULL publications | 5,230,000 | 0 |
| Call slice work | 831,990,000 | 826,760,000 |
| Call instructions | 50,000 | 50,000 |
| Allocations/allocated bytes, numeric + calls + GC | 0 / 0 | 0 / 0 |
| Peak shared charged bytes, calls | 7,078,197 | 7,078,197 |
| Idle active root ranges | 34 | 34 |
| Retained FrameSlots bytes | 289,024 | 289,024 |
| Live-native GC work / descriptors / Values | 9,837 / 45 / 8,766 | 9,837 / 45 / 8,766 |
| Live-native GC time, ns | 92,789 | 93,391 |
| Owner cancellation work / slices | 46,169 / 4 | 46,169 / 4 |
| Owner cancellation time, ns | 6,449 | 6,700 |

Median call total improves by 83.4% in this load. Numeric timings overlap and
their +3.1% median variation provides no evidence of a numeric-loop improvement.
GC and cancellation perform identical counted work; timing differences are host
variation. Most reported call work is conservative window construction/release
accounting, including O(max_root_ranges) free-span manipulation, so the removed
5.23 million real dispatch steps are a small fraction of that conservative sum.

Fault nodes enlarge the preallocated Heap slab. The earlier incomplete-fault
run charged ~4.37 MB, while both complete-fault variants charge ~7.08 MB. This is
the explicit memory cost of the inline private graph metadata, independent of
the NULL-loop optimization. Fault capture itself creates slab objects, consumes
logical allocation quotas and GC edges, and is not included in this echo load.

## Reproduction

At each named implementation revision, build before timing:

```
cargo build -p aside-vm --release --example executor_load
target/release/examples/executor_load
```

Save the first executable, rebuild the optimized revision and alternate the two
executables seven times. The raw JSONL records variant, pair number and complete
stdout. Author self-tests verify real repeated calls allocate zero times, logical
span reads preserve bitmap errors, and callback/continued waits still preserve
values, faults, identity and root lifetime with instruction budget 1 and GC.
This comparison is author evidence; independent V03 execution remains separate.

## Native-default integration check

Seven additional standalone runs at native-default implementation `cc55dd2`
(B01 `57ff60e`) repeat the same no-default echo load. This is a regression check,
not a new matched baseline/optimized comparison; the original comparison above
keeps identical fault semantics on both sides. Raw outputs are in
[CURRENT-PERF-SAMPLES.jsonl](CURRENT-PERF-SAMPLES.jsonl).

The current median numeric total is 325,686 µs (308,096–359,076); call total
39,841 µs (38,564–44,439); within-run call p50/p95 medians are 3,705/3,876 ns.
Every run still has zero numeric/call/GC allocator calls, zero redundant NULL
publications, 34 idle root ranges, and 289,024 retained FrameSlots bytes. Peak
shared charge is now 7,143,765 bytes, including the larger native/frame state
and B01 cached layout. Call work is 826,980,000; live-native GC still reports
9,837 units / 45 descriptors / 8,766 Values; cancellation still reports 46,169
conservative work in four slices (median 6,660 ns).

These timing fields cover no-default Ready/Pending dispatch. Script/native
defaults, shared closure callbacks and fault propagation are exercised by the
author semantic tests; this echo load is not a claim that closure-producing
defaults allocate zero times.

## Live work and retry progress integration

The owner clarified the budget contract in `8dfa3bc`. Implementation `d9032a9`
replaces configured descriptor capacity in each call's work estimate with the
current free-span count and its conservative growth across multiple releases
or partial-open rollback. Physical window initialization is still charged for
the entire actual window length; pool storage stays fully capacity-charged.
Script release charges its unsaved catch scan and successor together, so an
exact reported retry grant advances one bounded action. A regression executes
a Pending helper inside a live outer catch with work=1/allocation=0 adaptive
grants and GC=1, and gets identical call work with descriptor capacities 64 and
4,096. Another exercises a real partial-window rollback at hard root capacity.

Single-root GC and owner scans now use their actual high-water index, including
sentinel and released holes, consistent with V02's range-descriptor scan.
Seven standalone author runs are retained in
[LIVE-WORK-PERF-SAMPLES.jsonl](LIVE-WORK-PERF-SAMPLES.jsonl):

| Current measurement | Median or invariant |
| --- | ---: |
| Million-iteration numeric total, µs | 322,043 (305,890–420,614) |
| 10,000 Ready/Pending calls total, µs | 43,375 (40,759–52,345) |
| Within-run call p50/p95 medians, ns | 3,996 / 4,196 |
| Numeric / call work | 3,000,151 / 16,670,000 |
| Allocations/allocated bytes, numeric + calls + GC | 0 / 0 |
| Peak shared charged bytes | 7,143,773 |
| Idle root ranges / retained FrameSlots bytes | 34 / 289,024 |
| Live-native GC units / descriptors / Values | 8,812 / 45 / 8,766 |
| Live-native GC median time, ns | 89,204 |
| Owner cancellation work / slices | 296 / 1 |
| Owner cancellation median time, ns | 1,112 |

The work decrease corrects conservative accounting; it is not a CPU speedup
claim. Call timing ranges overlap the preceding integration runs. The GC
reduces 1,025 never-used single-root index checks in this load while preserving
all range and Value scans. Tests with live single roots and released holes
verify the new scan bound independently of reserved descriptor capacity.
These runs retain the same no-default echo scope and author-evidence limits.

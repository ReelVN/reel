# Review findings on 4fef7b6

Implementation: `2501e9be94c36c1d3a46f551b482cb71bdc3ed04`.
This addresses the three source findings in
[issue #22](https://github.com/jhq223/reel/issues/22), against the same B01
`57ff60e`, V02 `cceb151` and language contract `8dfa3bc`. Changes are confined
to executor code, private author regressions and executor evidence. Public
interfaces and the B01 wire format are unchanged.

## Release billing order

Previously Release searched caught records before admitting its work. It now
computes `caught + max(caught + 2, release_cost(4) + commit_work(ret))` from
constant-time metadata and admits that entire bound before calling any catch
predicate. The bound covers both removing an owned catch record and committing
the return/releasing the frame, including the unsaved initial scan. Repeated
insufficient grants perform no hidden catch scan; an exact reported grant can
advance one bounded action without having to repay unsaved work separately.

The author regression reaches a real helper Return under an active outer catch
by execution, then grants zero and repeatedly grants required_work minus one.
A test-only counter in the actual Release predicate stays at zero. Granting
exactly the reported work performs one predicate check and releases the helper
frame, while the outer catch and its fault remain active. No fabricated phase
is installed.

## Native layout admission

Vm load now derives one has-cells boolean for each native from the verified
shared-signature table. Its vector capacity and inline owner use the common
Heap ledger; the cache is fixed with the immutable loaded program. Both call
cost estimation and native construction read it by NativeId in O(1), avoiding
the former pre-admission binary query. The fixture's one boolean owns one byte
in addition to the already charged VM control metadata.

Per-parameter B01 layout queries still run in Defaults/Project, after the
stage's query work has been admitted. A test-only counter records actual query
invocations, rather than claiming to count the private B01 binary predicates.
Rejected native call grants perform no query and publish no new roots; repeated
rejected Defaults grants perform no query. Its exact reported grant performs
one query and advances to the first Cell parameter stage. Source inspection
finds has-cells table traversal only in load, and parameter table traversal
only behind this admitted runtime wrapper.

## Lexical module and prepared binding ownership

Previously Prefix copied the native caller's module into the default frame.
The ScriptFrame now always retains its FunctionDef's lexical module for code,
source/stack identity, local Cell contracts and closures. A separate bounded
inline parameter_module records the binder's module for validation/reuse of
shared prefix Cells. Ordinary script calls establish that parameter ownership
in their own lexical module; internal prefixes carry it forward.

Two executed cross-module variants use caller A and native thunks defined in B:

- A Value-only prefix throws before native Entry. Its complete fault has the B
  thunk's code, PC and source map, and the original stack reports B for that
  script frame followed by A for the native boundary/caller. Entry remains zero;
  delivery keeps the graph alive through GC and discarding it reclaims the graph.
- A Cell prefix mutates the shared parameter and creates a local Cell plus a
  closure in B. The closure retains the exact prepared parameter Cell from A
  and the local Cell's B contract; its module is B. Both contracts retain their
  code/module versions. The native author-value view becomes [43, 42, 43] before
  its single Entry; Pending resumes once, and task delivery disposal/GC releases
  both Cells and restores the pooled root count.

## Author evidence and limits

The four regressions are in [review_tests.rs](review_tests.rs). They use real
hand-authored verified B01 packages, instruction/work budgets starting at one,
allocation starting at zero, adaptive reported grants and interleaved GC units
of one. Test-only counters are absent from production builds.

At the implementation revision:

```
cargo test -p aside-vm --lib executor::review_tests -- --nocapture
cargo test -p aside-vm --quiet
cargo clippy -p aside-vm --all-targets -- -D warnings
cargo check -p aside-vm --tests --examples --target i686-unknown-linux-gnu
```

All four targeted regressions passed. Full checks passed 63 unit, 55 executor,
15 FrameSlots, 16 storage and one doc test, Clippy with warnings denied, and
i686 compilation. The 32-bit target was not linked/run.

Seven release probe runs retain zero allocator calls/bytes for numeric/call/GC
work, 34 idle root ranges and 289,024 FrameSlots bytes. Call median is 41,949 µs
for 10,000 Ready/Pending calls (41,317–43,801); million-iteration numeric median
is 315,506 µs (312,378–363,558). Call work stays 16,670,000; GC stays 8,812 units,
45 descriptors and 8,766 Values; cancellation stays 296 work in one slice.
Peak shared charge is 7,143,814 bytes, 41 bytes above the preceding production
measurement for the new cache owner and boolean. These are standalone host
regressions, not a matched speedup comparison. Defaults/faults have semantic
coverage above and are not part of the no-default echo allocation load. Raw
outputs: [REVIEW-FIX-PERF-SAMPLES.jsonl](REVIEW-FIX-PERF-SAMPLES.jsonl).

This continuation is authorized implementation and author testing. The earlier
platform-blocked independent V03 execution remains separately pending and is
neither replaced nor bypassed by these checks. Source lowering/execution and
device acceptance are not claimed. A later Exit review at `1af4d31` admits and
saves handler selection before application, and removes the hypothetical
release fee on ordinary exits; see [Exit review evidence](EXIT-BUDGET-REVIEW.md).
Bounded destruction of a final retired
CodeLease and the other integration follow-ups remain as documented in
[FAULT-CONTRACT.md](FAULT-CONTRACT.md).

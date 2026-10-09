# Exit selection admission and saved application

Implementation: `1af4d312e10c561f23c33e5441f85c0758b1a56d`.
Dependencies remain B01 `57ff60e`, V02 `cceb151` and owner language contract
`8dfa3bc`; the draft PR's creation-time base is unchanged. Changes are limited
to executor state, cancellation handling, private author tests and evidence.

## Charged selection, then application

Previously `ScriptPhase::Exit` called `replacement_needed` before admitting
work. That helper invoked the shared B01 handler selector, scanning the
function's exception regions. Repeated work/allocation shortages could repeat
this unpaid scan before the eventual charged exit action.

Exit now computes its selection bound from the constant-time region count and
admits it before invoking either B01 `exception_handler` or `cleanup_handler`.
The bound is `regions * 8 + 1`, or `regions * 32 + 1` when a consumed region
requires strict ancestry comparisons between at most three parent and three
child blocks. Selection remains an atomic O(actual regions) action; an
undersized grant performs no query. This is the existing shared selector,
without another opcode format or locally reimplemented region relationships.

The result, including None, is saved inline in `ScriptPhase::ExitApply` together
with the original PC/reason. Later grants do not rescan regions. Loaded code,
PC, exit reason and the consumed-region result remain fixed while application
removes superseded unwind/catch records or waits for allocation/capacity. The
inline state belongs to the existing capacity-charged frame pool; it has no
owned allocation or unbounded map. Existing environment/unwind windows retain
all Values and complete fault graphs through GC.

Application reads only the last unwind/range to determine replacement in O(1).
It admits `2 * caught_count + 12`, plus the actual root-action bound and, when
appending a replaced-fault cause, a code-owned source binary-search bound and
`FAULT_NODE_BYTES`. The catch bound covers departure search plus Vec removal,
or departure search plus existing-handler search. At most one superseded
unwind is removed per action. Root work is `release_cost(1)` for an actual
replacement, `window_push_cost(1, 2)` for finally entry, and zero otherwise.
Frame teardown remains a separately admitted Release action. Thus ordinary
Exit no longer prepays a hypothetical unwind-root release.

Admission precedes cause-event assignment and all application side effects.
Hard Heap capacity can fail after a stable replacement event is assigned;
the saved handler, complete fault, old exit root and event survive, and a
later successful retry appends that cause once. Cooperative cancellation
recognizes ExitApply's complete prior fault, retains it as ExplicitCause,
invalidates the saved selection and selects anew for the new Cancelled fault.
Deadlines are checked between these stages. Neither the atomic selector nor
the existing conservative free-span release bound is described as constant
CPU work or a hard wall-clock deadline guarantee.

## Author regressions

The five tests in [exit_tests.rs](exit_tests.rs) execute verified hand-authored
B01 packages to reach their real stages; they do not install fabricated phases.
Instruction/work budgets start at one, allocation at zero, with adaptive
reported grants and interleaved GC units of one. A private test-only counter
counts actual shared selector invocations, not B01's internal predicates.

- A finally Throw with 65 valid regions executes zero selector calls under
  repeated rejected selection grants. Its exact selection grant saves one
  result. Repeated work/allocation shortages then perform no second query,
  preserve the fault/root and append ReplacedExit(Return) once on admission.
- Repeated ResourceBlocked at hard object capacity keeps that same selection,
  fault, event and old Return Cell. Releasing a rooted host blocker and GC
  permits completion; delivery roots retain the graph until explicit disposal.
- An inner catch within finally requires no replacement allocation, retains
  the original pending Return and ultimately returns its exact Cell.
- Cancellation during saved application performs one fresh selection and
  retains both the replaced Return and the complete prior Thrown fault.
- An ordinary Return with no regions/unwinds admits selection with work=1
  and application with work=12/allocation=0. Its roots remain until the actual
  separately admitted frame release.

At the implementation revision, all five targeted regressions passed. Full
`cargo test -p aside-vm --quiet` passed 68 unit, 55 executor, 15 FrameSlots,
16 storage and one doc test. Clippy all-targets with warnings denied and
the i686 tests/examples compile check passed. i686 was not linked or run.

## Release regression measurements

Seven standalone release runs at this exact code revision are retained in
[EXIT-BUDGET-PERF-SAMPLES.jsonl](EXIT-BUDGET-PERF-SAMPLES.jsonl).

| Measurement | Median or invariant |
| --- | ---: |
| Million-iteration numeric total, µs | 304,158 (273,131–349,791) |
| 10,000 Ready/Pending calls total, µs | 41,667 (38,574–43,871) |
| Within-run call p50/p95 medians, ns | 3,766 / 3,966 |
| Numeric instructions / work | 3,000,004 / 3,000,136 |
| Call instructions / work | 50,000 / 16,370,000 |
| Numeric/call/GC allocator calls and bytes | 0 / 0 |
| Peak shared charged bytes | 7,143,814 |
| Idle root ranges / retained FrameSlots bytes | 34 / 289,024 |
| Live-native GC units / descriptors / Values | 8,812 / 45 / 8,766 |
| GC median time, ns | 89,335 |
| Owner cancellation work / slices | 296 / 1 |
| Owner cancellation median time, ns | 1,072 |

Call work decreases from 16,670,000 solely by removing the hypothetical
ordinary-exit release fee; memory/root residency remains unchanged. These
standalone host samples are a regression record, not a matched CPU speedup
comparison or device guarantee. One numeric slice maximum was 16.24 ms;
host scheduling is included. The no-default echo load does not measure fault
or default allocation; the semantic regressions above cover those paths.

This is authorized implementation and author evidence. The platform-blocked
independent V03 execution remains separately pending and is neither replaced
nor bypassed. Source lowering/execution is not claimed. Remaining handler,
standard-library, snapshot/restore, invoke_sync and bounded final CodeLease
retirement integrations are documented in [FAULT-CONTRACT.md](FAULT-CONTRACT.md).

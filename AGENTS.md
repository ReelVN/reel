# Reel development rules

## Source of truth

Read `docs/engine-architecture.md`, `docs/aside-language.md`, and the relevant section of `docs/guide/zh-CN.md` or `docs/guide/en.md` before editing code. These documents define one shared design. Propose and resolve interface changes in the documents before implementing a different behavior.

## Scope

Each task must identify its outcome, allowed directories, interface version, dependencies, behavior checks, and relevant performance limits. Keep changes within that scope. Report a concrete interface conflict to the integration owner rather than inventing a parallel API.

Use the existing module boundaries. Aside remains independent of Reel and platform libraries. Platform adapters contain native handles. Author-facing UI, portraits, text, and effects use the common stage model.

## Dependencies

Use pinned ordinary dependencies by default. Maintain required borrowed implementations in workspace crates, with provenance, licenses, and a concise local change record. Preserve the Quirrel test inputs and original oracles under compiler fixtures until their valuable behavior is migrated. Do not reintroduce third_party or duplicate runtime implementations. Avoid nested Git repositories and duplicate sources for the same UI types.

## Implementation quality

- Implement the agreed behavior; do not report placeholder success as a feature.
- Keep input ownership, cancellation, memory ownership, terminal results, and snapshot restoration explicit.
- Validate at the agreed boundaries and avoid repeated speculative defensive layers.
- Keep real-time audio free of allocation, file IO, and main-thread waits.
- Use bounded queues and the documented resource accounting.
- Measure relevant behavior and cost with reproducible commands. A test count is not evidence of correctness or performance.
- Keep diagnostics tied to the user's source location.

## Repository hygiene

- Maintain borrowed UI code in the single `crates/reel-ui` workspace package, organized by responsibility through modules. Reel owns subsequent changes; upstream synchronization, contribution, package versions and source-layout closure are not maintenance requirements. Preserve the original Apache license, necessary copyright/NOTICE and concise provenance. Keep production `unsafe_code = "forbid"` and native window/GPU dependencies outside UI.
- First-party validation tooling belongs in Rust tests or `crates/reel-dev`. Keep unchanged upstream tools with their documented source snapshot; do not use them to hide a second first-party implementation.
- Write reproducible logs, measurements, screenshots and generated reports under ignored `target/reel-artifacts/` or temporary storage. Track only source, concise contracts and necessary immutable test inputs. Update consumers and preserve assertions before retiring a fixture or validation tool.
- Keep documentation linked to current commands; historical measurements use fixed commit links and are not current verification claims.

## Documentation

The project lead maintains the game-creator guide. A change to syntax, API, defaults, or behavior must include the corresponding specification and both creator-guide editions in the same PR. Their Aside snippets must match. Mark implementation status honestly and link the exact runnable example once supported.

## Review and delivery

Use small, independently reviewable PRs in draft mode while work is pending. Include the outcome, changed interfaces, actual verification commands/results, and remaining limitations. Keep the issue description current; comments are for decisions, blockers, review, and completion.

The integration owner reads critical code and reruns relevant checks before acceptance. Agent reports are inputs to that review, not acceptance by themselves. Platform/device behavior is marked verified only on the relevant target. A demo or complete playable story is not a prerequisite; focused contract checks are.

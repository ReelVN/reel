# Reel development rules

## Source of truth

Read `docs/engine-architecture.md` and `docs/aside-language.md` before editing code. These documents define the shared engine and language contracts. Propose and resolve interface changes in the documents before implementing a different behavior.

## Scope

Each task must identify its outcome, allowed directories, interface version, dependencies, behavior checks, and relevant performance limits. Keep changes within that scope. Report a concrete interface conflict to the integration owner rather than inventing a parallel API.

Use the existing module boundaries. Aside remains independent of Reel and platform libraries. Platform adapters contain native handles. Author-facing UI, portraits, text, and effects use the common stage model.

## Dependencies

Use pinned ordinary dependencies by default. Maintain required borrowed implementations in workspace crates, with provenance, licenses, and a concise local change record. Preserve the Quirrel test inputs and original oracles under compiler fixtures until their valuable behavior is migrated. Keep each maintained implementation in one workspace crate, with a single source for shared UI types.

## Implementation quality

- Implement the agreed behavior; do not report placeholder success as a feature.
- Keep input ownership, cancellation, memory ownership, terminal results, and snapshot restoration explicit.
- Validate at the agreed boundaries and avoid repeated speculative defensive layers.
- Keep real-time audio free of allocation, file IO, and main-thread waits.
- Use bounded queues and the documented resource accounting.
- Measure relevant behavior and cost with reproducible commands. A test count is not evidence of correctness or performance.
- Keep diagnostics tied to the user's source location.

## Repository hygiene

- Keep Rust implementation files within 1,500 lines; files containing tests may reach 2,000 lines. Split modules by responsibility, using idiomatic Rust module and file names. Place unit tests with their owning module, integration tests under `tests/`, and shared test helpers in dedicated modules.
- Maintain borrowed UI code in the single `crates/reel-ui` workspace package, organized by responsibility through modules. Reel maintains subsequent changes according to its module responsibilities. Preserve the original Apache license, necessary copyright/NOTICE and concise provenance. Keep production `unsafe_code = "forbid"` and native window/GPU dependencies outside UI.
- First-party validation tooling belongs in Rust tests or `crates/reel-dev`. Keep unchanged upstream tools with their documented source snapshot; do not use them to hide a second first-party implementation.
- Write reproducible logs, measurements, screenshots and generated reports under ignored `target/reel-artifacts/` or temporary storage. Track only source, concise contracts and necessary immutable test inputs. Update consumers and preserve assertions before retiring a fixture or validation tool.
- Keep documentation linked to current commands; historical measurements use fixed commit links and are not current verification claims.

## Documentation

Maintain the English `CHANGELOG.md` for user-visible changes under `Unreleased`. When preparing a version baseline or release, move those entries into its dated version section.

Public interfaces carry English source documentation beside their declarations. Native macros and property schemas expose the same documentation to editor tooling; executable Aside examples in these comments are checked against the registered interfaces. Update these declarations with API, default and behavior changes.

The project lead maintains engine and language contracts in `docs/` and publishes game development tutorials at [reelvn.com](https://reelvn.com). Keep equivalent tutorial snippets identical in both editions. Preserve behavior assertions in Rust tests when moving examples out of Markdown. The compiler uses `crates/aside-compiler/tests/fixtures/author_scenes.md` for retained author-scene inputs. Verification logs and maintenance reports belong under ignored artifacts. Record completed work in its PR.

## Review and delivery

Use small, independently reviewable PRs in draft mode while work is pending. Include the outcome, changed interfaces, actual verification commands/results, and remaining limitations. Keep the issue description current; comments are for decisions, blockers, review, and completion.

The integration owner reads critical code and reruns relevant checks before acceptance. Agent reports are inputs to that review, not acceptance by themselves. Platform/device behavior is marked verified only on the relevant target. A demo or complete playable story is not a prerequisite; focused contract checks are.

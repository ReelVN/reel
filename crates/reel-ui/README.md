# Reel UI

One platform-independent UI package, maintained by Reel from borrowed Masonry
code. [Source and license](ORIGIN.md) · [Input contract](INTERFACE.md) ·
[Frame/resource contract](R01-INTERFACE.md) · [Historical R01 evidence](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-ui/R01-REPORT.md).

| Module | Responsibility |
| --- | --- |
| `core` | Widget, contexts, properties, events and live widget ownership |
| `app` | RenderRoot, layers and frozen presentation geometry |
| `layout` | Size/length definitions and measurement cache |
| `properties` | Unified layout/paint/text/control property definitions |
| `widgets`, `theme` | Button/Image/Label/Slider and default appearance |
| `bridge` | Candidate identity, input policy and presentation acceptance |
| `frame` | Bounded portable drawing export and immutable RenderFrame |
| `resources` | Prepared resources, versions, budgets and leases |
| private `passes` | Runtime layout/update/input/paint traversal |
| private `tree_arena` | Safe widget tree and leased bounded lookup scratch |
| `doc`, `util` | Maintained concepts and supporting UI helpers |

Text shaping and paragraph layout come from the shared
[`reel-text`](../reel-text/README.md) crate, re-exported as `reel_ui::reel_text`.
UI widgets remain in this package.

`reel-ui → reel-core/reel-text`; `reel-render → reel-ui/core`;
`reel-player → reel-ui/render/core`. UI never depends on renderer, player,
winit or wgpu, including when their native features are enabled.
The crate inherits the workspace version, is unpublished and forbids unsafe code.
There are no compatibility packages or old package-name modules.

Run from the repository root with Rust 1.96.0 and Fontconfig/pkg-config:

```sh
cargo fmt --all --check
cargo clippy --locked -p reel-ui -p reel-render -p reel-player -p reel-core --all-targets --all-features -- -D warnings
cargo test --locked -p reel-ui -p reel-render -p reel-player -p reel-core --no-default-features --features reel-core/test-support -- --test-threads=1
cargo test --locked -p reel-ui -p reel-render -p reel-player -p reel-core --no-default-features --features reel-core/test-support --release -- --test-threads=1
cargo run --locked -p reel-dev -- audit-ui
cargo doc --locked -p reel-ui --no-deps
```

`reel-core/test-support` enables only the core contract test fixtures, so those
tests run while the production UI/window/GPU default features remain off.

`tests/lookup_depth.rs` includes the production private safe tree in an isolated
allocator-instrumented binary. All deep/small-stack/capacity/zero-allocation
probes and busy-workspace fallback assertions are retained. Pointer allocation
probes keep their separate binary. `reel-dev measure` selects the exact `reel-ui`
Cargo executable for `contracts` and `r01_measurements`.

Widget documentation lives in each widget's Rust source. Shared concepts and
pass descriptions live in `src/doc` as Rust modules. Original images: [button](screenshots/button_hello.png),
[label](screenshots/label_styled_label.png), [slider](screenshots/slider_initial_state.png).

Generated logs and measurements go under ignored `target/reel-artifacts/`.
Native device tests and window Expose/recovery commands are documented in the
[R02 report](https://github.com/jhq223/reel/blob/355571b18bd4024914438a7ac6bd94ebf3c7826f/crates/reel-render/R02-REPORT.md). Those require a Vulkan device and an
actual window/display; software Vulkan/Xvfb results establish only that environment.
The shared author stage and Aside composition are connected through reel-engine
and reel-player, including author images, sliders and toggles. See the
[creator guide](../../docs/guide/en.md#value-controls-and-settings) for state and callback contracts.
Geometry export uses an explicit traversal stack. Node counts and path depth
follow the recorded tree; input route storage grows before publication within
its memory account, preserving the accepted view and active pointer capture.

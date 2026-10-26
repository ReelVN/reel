# Authoring workflow contracts

Scope: `reel-cli`, `reel-engine`, `reel-player`, `reel-media`, their focused tests and source documentation, matching creator examples. Existing `reel-audio` playback-rate control is reused. Aside syntax and platform ownership remain unchanged. ASB/host ABI and snapshot format remain v1; snapshots require matching code as documented.

## Resource-complete scenario execution

Self-checking projects execute the ordinary `main::main` entry. Source preparation uses the same resource conversion and effect preparation as `dev`. The packaged path publishes catalogs and effect archives through the existing distribution builder, then loads that package through the player distribution loader. Fonts, images, media and localization share the resulting asset context. Generated artifacts belong to a temporary directory or the invoking project's ignored build directory.

Checks cover translated text, a typed fragment effect, ordinary image and audio resources in source and packaged execution. Existing assertions and resource bounds remain in force. Rendering submission and image capture are distinct choices.

Run the portable scenario checks with `cargo test -p reel-cli --test project_scenarios`. For an external self-checking project, set `REEL_SCENARIO` to its directory and run `cargo test -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture`. Each scenario runs source and a release export with archive-backed resources. Temporary files use the host temporary directory; sandboxed environments can select writable storage with `TMP`/`TEMP` or `TMPDIR`.

`REEL_SCENARIO_GPU=1` submits frames to the GPU without enabling image capture. `REEL_SCENARIO_GPU_CAPTURE=1` additionally enables authored `stage.capture` requests and capture-ordinal pointer scripts. PNG files are written only when `REEL_SCENARIO_CAPTURE_DIR` is set. `REEL_SCENARIO_TIMEOUT_SECS` bounds each run; the entry remains `main::main`.

## Audio playback rate

`audio.play(..., rate: 1)` starts an activity at a positive finite content rate. `audio.rate(activity, value, duration: 0)` changes that activity's rate, optionally using a linear ramp in elapsed playback time. A rate of 2 consumes twice as much source content per output second and raises pitch by an octave. Pause freezes content and the rate ramp. Content positions, seek endpoints, loop bounds and cue targets retain source seconds.

Queues retain one transport rate across their item boundaries. Snapshots preserve the current rate and remaining ramp; restore reconstructs the transport at its saved source position. Owner retirement cancels playback through the existing activity path. Audio callback work uses existing bounded commands and preallocated resampling storage; it performs no allocation, file IO, blocking wait or PCM destruction.

Checks measure content consumption at 0.5/1/2 rates, audible output, timed changes, pause, loop and queue transitions, cancellation and restore. Supported rates must remain bounded by the documented mixer work limits; rejected input reports the caller's source location.

## Shared interaction appearance

`appearance(base: {}, hover: {}, focus_visible: {}, pressed: {}, disabled: {})` creates an immutable, shareable set of common stage appearance properties. A node accepts it through `appearance:`. Only visual properties participate; callbacks, ownership, enabled/visible flags and layout dimensions remain ordinary node properties. Unknown states or unsupported properties report an error at author input.

Precedence is base, hover, focus-visible, pressed, disabled, followed by explicit scope overrides. Unspecified properties retain the node's ordinary values. Appearance selection changes projected values without writing back to model bindings, replacing nodes or restarting input. The same value works on buttons, input regions and other stage nodes; button captions use the selected text properties.

Construction validates and copies the property data once. Sharing a value shares immutable data; interaction changes select prepared property entries. Static frames require no polling tasks or reconstruction. Snapshot restoration retains the appearance value and recomputes the applicable input state through the ordinary stage path.

Checks cover combined state precedence, release after a pressed visual change, ancestor disabling, model-bound base values, scope overrides, source/ASB execution and restoration. Compare an authored decorated button with the current multi-node/derived-property construction using reproducible allocation and projection measurements.

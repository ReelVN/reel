#!/bin/sh
set -eu
# Run from repository root. GPU tests require a real adapter and a real font.
cargo fmt --all -- --check
cargo clippy -p reel-core -p reel-stage -p reel-render -p reel-player --all-targets --all-features --locked -- -D warnings
cargo test -p reel-core -p reel-stage -p reel-render -p reel-player --all-features --locked -- --test-threads=1
cargo test -p reel-core -p reel-stage -p reel-render -p reel-player --all-features --locked --release -- --test-threads=1
cargo check -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features --locked
cargo tree -p reel-core -p reel-stage -p reel-render -p reel-player --no-default-features -e normal
if [ "${REEL_RUN_GPU_TESTS:-0}" = 1 ]; then
    : "${REEL_R01_TEST_FONT:?set a real TTF/OTF path}"
    cargo test -p reel-render --features wgpu --locked --test gpu_pixels -- --ignored --nocapture --test-threads=1
    cargo test -p reel-render --features wgpu --locked --release --test gpu_pixels -- --ignored --nocapture --test-threads=1
    cargo test -p reel-stage --locked actual_parley_label_glyphs_are_copied_without_reshaping -- --ignored --nocapture
fi

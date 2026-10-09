#!/usr/bin/env bash
set -euo pipefail
export RUSTUP_TOOLCHAIN=1.96.0
probe_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd -- "$probe_dir/../.." && pwd)"
cd -- "$repo_root"
: "${1:?usage: check.sh /path/to/extracted-xilem-271a27a6}"
export CARGO_TARGET_DIR="$repo_root/target/reel-artifacts/masonry-build"
experiment="$repo_root/target/reel-artifacts/masonry-experiment/Cargo.toml"
cargo fmt --manifest-path "$probe_dir/Cargo.toml" --check
cargo clippy --locked --manifest-path "$probe_dir/Cargo.toml" --all-targets -- -D warnings
cargo test --locked --manifest-path "$probe_dir/Cargo.toml"
cargo run --locked --manifest-path "$probe_dir/Cargo.toml"
cargo run --locked -p reel-dev -- prepare-masonry "$1"
cargo clippy --locked --manifest-path "$experiment" --features experimental --all-targets -- -D warnings
cargo test --locked --manifest-path "$experiment" --features experimental
cargo run --locked --release --manifest-path "$experiment" --features experimental
cargo run --locked -p reel-dev -- deps "$probe_dir/Cargo.toml"
cargo run --locked -p reel-dev -- deps "$experiment"

#!/usr/bin/env bash
set -euo pipefail
# The manifest path does not change rustup's working-directory toolchain choice.
# Pin child Cargo/Python processes even when invoked from a root using Rust 1.90.
export RUSTUP_TOOLCHAIN=1.96.0
probe_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cargo --version
rustc --version
cargo fmt --manifest-path "$probe_dir/Cargo.toml" --check
cargo clippy --locked --manifest-path "$probe_dir/Cargo.toml" --all-targets -- -D warnings
cargo test --locked --manifest-path "$probe_dir/Cargo.toml"
cargo run --locked --manifest-path "$probe_dir/Cargo.toml"
python3 "$probe_dir/scripts/prepare.py" "$@"
cargo clippy --locked --manifest-path "$probe_dir/experiment/Cargo.toml" --features experimental --all-targets --target-dir "$probe_dir/target" -- -D warnings
cargo test --locked --manifest-path "$probe_dir/experiment/Cargo.toml" --features experimental --target-dir "$probe_dir/target"
cargo run --locked --release --manifest-path "$probe_dir/experiment/Cargo.toml" --features experimental --target-dir "$probe_dir/target"
python3 "$probe_dir/scripts/check-deps.py"

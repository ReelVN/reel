#!/usr/bin/env bash
set -euo pipefail
probe_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cargo fmt --manifest-path "$probe_dir/Cargo.toml" --check
cargo test --locked --manifest-path "$probe_dir/Cargo.toml"
cargo run --locked --manifest-path "$probe_dir/Cargo.toml"
python3 "$probe_dir/scripts/prepare.py" "$@"
cargo test --locked --manifest-path "$probe_dir/experiment/Cargo.toml" --features experimental --target-dir "$probe_dir/target"
cargo run --locked --release --manifest-path "$probe_dir/experiment/Cargo.toml" --features experimental --target-dir "$probe_dir/target"
python3 "$probe_dir/scripts/check-deps.py"

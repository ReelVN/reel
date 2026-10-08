#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(cd -- "$script_dir/../../../.." && pwd)"
python3 "$script_dir/verify_kira_fork.py"
kira_test_dir="$(mktemp -d "${TMPDIR:-/tmp}/reel-kira-test.XXXXXX")"
trap 'rm -rf -- "$kira_test_dir"' EXIT
# Kira is excluded from engine workspace membership. Test the exact fork copy
# independently so dev dependencies/features never enter shipping reel-media.
cp -a -- "$repo_root/third_party/kira/." "$kira_test_dir/"
cp -- "$script_dir/kira-test.lock" "$kira_test_dir/Cargo.lock"
cargo test --manifest-path "$kira_test_dir/Cargo.toml" --no-default-features \
    --locked --lib reel_retirement -- --nocapture

#!/usr/bin/env bash
set -euo pipefail

probe_script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(cd -- "$probe_script_dir/../../../.." && pwd)"
probe_target_dir="$(mktemp -d "${TMPDIR:-/tmp}/reel-dealloc.XXXXXX")"
trap 'rm -rf -- "$probe_target_dir"' EXIT
cd -- "$repo_root"

# Build the unchanged production crate with its normal unsafe_code=forbid lint.
# Compile only this standalone allocator observer separately, into the temp dir.
for profile in debug release; do
    build_args=()
    rustc_args=(--edition 2024 --test)
    if [[ "$profile" == release ]]; then
        build_args+=(--release)
        rustc_args+=(-O)
    fi
    cargo build -p reel-core --features test-support --locked \
        --target-dir "$probe_target_dir" "${build_args[@]}"
    libraries=("$probe_target_dir/$profile/deps/"libreel_core-*.rlib)
    [[ ${#libraries[@]} == 1 && -f "${libraries[0]}" ]]
    rustc "${rustc_args[@]}" "$probe_script_dir/dealloc_probe.rs" \
        --extern "reel_core=${libraries[0]}" \
        -L "dependency=$probe_target_dir/$profile/deps" \
        -o "$probe_target_dir/probe-$profile"
    "$probe_target_dir/probe-$profile" --nocapture
done

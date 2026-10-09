#!/usr/bin/env bash
set -euo pipefail

probe_script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(cd -- "$probe_script_dir/../../../.." && pwd)"
mkdir -p "$repo_root/target/reel-artifacts"
probe_target_dir="$(mktemp -d "$repo_root/target/reel-artifacts/aside-root-dealloc.XXXXXX")"
trap 'rm -rf -- "$probe_target_dir"' EXIT
cd -- "$repo_root"
for profile in debug release; do
    build_args=()
    rustc_args=(--edition 2024 --test)
    if [[ "$profile" == release ]]; then
        build_args+=(--release)
        rustc_args+=(-O)
    fi
    cargo build -p aside-vm --locked --target-dir "$probe_target_dir" "${build_args[@]}"
    libraries=("$probe_target_dir/$profile/deps/"libaside_vm-*.rlib)
    [[ ${#libraries[@]} == 1 && -f "${libraries[0]}" ]]
    rustc "${rustc_args[@]}" "$probe_script_dir/root_range_dealloc_probe.rs" \
        --extern "aside_vm=${libraries[0]}" -L "dependency=$probe_target_dir/$profile/deps" \
        -o "$probe_target_dir/probe-$profile"
    "$probe_target_dir/probe-$profile" --nocapture --test-threads=1
done

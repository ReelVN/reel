#!/usr/bin/env bash
set -euo pipefail
probe_script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(cd -- "$probe_script_dir/../../../.." && pwd)"
probe_target_dir="$(mktemp -d "${TMPDIR:-/tmp}/reel-pcm.XXXXXX")"
trap 'rm -rf -- "$probe_target_dir"' EXIT
cd -- "$repo_root"
for profile in debug release; do
    build_args=()
    rustc_args=(--edition 2024)
    if [[ "$profile" == release ]]; then build_args+=(--release); rustc_args+=(-O); fi
    cargo build -p reel-media --no-default-features --locked --target-dir "$probe_target_dir" "${build_args[@]}"
    media_libs=("$probe_target_dir/$profile/deps/"libreel_media-*.rlib)
    core_libs=("$probe_target_dir/$profile/deps/"libreel_core-*.rlib)
    [[ ${#media_libs[@]} == 1 && ${#core_libs[@]} == 1 ]]
    for probe in pcm reclaim; do
        rustc "${rustc_args[@]}" "$probe_script_dir/${probe}_probe.rs" --extern "reel_media=${media_libs[0]}" --extern "reel_core=${core_libs[0]}" -L "dependency=$probe_target_dir/$profile/deps" -o "$probe_target_dir/$probe-$profile"
        printf 'profile=%s probe=%s\n' "$profile" "$probe"
        "$probe_target_dir/$probe-$profile"
    done
done

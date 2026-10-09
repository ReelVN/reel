#!/usr/bin/env python3
"""Isolate lookup from layout/build/drop and compare the six review probes."""
import argparse
from pathlib import Path
import resource
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--skip-baselines", action="store_true")
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parents[3]
tmp = Path(tempfile.mkdtemp(prefix="reel-tree-depth-"))
source = "third_party/masonry/tree_arena/src/tree_arena_safe.rs"
versions = [("current", (repo / source).read_text())]
if not args.skip_baselines:
    versions[:0] = [
        (name, subprocess.check_output(["git", "show", f"{ref}:{source}"], cwd=repo, text=True))
        for name, ref in [("old", "fbb3c79"), ("recursive", "2534b9a")]
    ]

main = r'''
extern crate alloc;
#[allow(dead_code)]
mod arena;
type NodeId = u64;
use arena::{ArenaMutList, TreeArena};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, hint::black_box, io::Write, time::Instant};
#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
fn build(mut children: ArenaMutList<'_, u64>, id: u64, depth: u64) {
    let node = children.insert(id, id);
    if id < depth { build(node.children, id + 1, depth); }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let op = args[1].clone();
    let depth: u64 = args[2].parse().unwrap();
    let stack: usize = args[3].parse().unwrap();
    let capacity: usize = args[4].parse().unwrap();
    let samples: usize = args[5].parse().unwrap();
    // Build on an independent large stack, then only lookup on the reported stack.
    let mut tree = std::thread::Builder::new().stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let mut tree = TreeArena::new();
            build(tree.roots_mut(), 1, depth);
            tree
        }).unwrap().join().unwrap();
    CONFIGURE
    println!("BUILT op={op} depth={depth} stack_bytes={stack} capacity={capacity}");
    std::io::stdout().flush().unwrap();
    std::thread::Builder::new().name("lookup-probe".into()).stack_size(stack)
        .spawn(move || {
            let mut elapsed = Vec::with_capacity(samples);
            let region = Region::new(ALLOCATOR);
            for _ in 0..samples {
                let start = Instant::now();
                if op == "find" {
                    assert_eq!(*black_box(tree.find(depth).unwrap().item), depth);
                } else {
                    *black_box(tree.find_mut(depth).unwrap().item) += 1;
                }
                elapsed.push(start.elapsed().as_nanos());
            }
            let stats = region.change();
            elapsed.sort_unstable();
            println!("OK op={op} depth={depth} stack_bytes={stack} capacity={capacity} samples={samples} allocations={} reallocations={} bytes={} p95_ns={} max_ns={}",
                stats.allocations, stats.reallocations, stats.bytes_allocated,
                elapsed[(samples * 95).div_ceil(100) - 1], elapsed[samples - 1]);
            // Exclude nested HashMap destruction from the lookup probe.
            std::mem::forget(tree);
        }).unwrap().join().unwrap();
}
'''

def no_core():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

probes = [
    ("debug", "find", 256, 64 * 1024),
    ("debug", "find_mut", 512, 64 * 1024),
    ("debug", "find", 512, 128 * 1024),
    ("debug", "find_mut", 1024, 128 * 1024),
    ("release", "find", 1024, 64 * 1024),
    ("release", "find_mut", 2048, 64 * 1024),
]
args.output.parent.mkdir(parents=True, exist_ok=True)
with args.output.open("w") as log:
    for version, content in versions:
        project = tmp / version
        (project / "src").mkdir(parents=True)
        (project / "src/arena.rs").write_text(content)
        configure = "if capacity > 0 { tree.configure_lookup_path_storage(capacity); }" if version == "current" else ""
        (project / "src/main.rs").write_text(main.replace("CONFIGURE", configure))
        (project / "Cargo.toml").write_text(f'''[package]
name = "tree-depth-{version}"
version = "0.0.0"
edition = "2024"
[features]
default = ["std"]
std = []
[dependencies]
hashbrown = {{ version = "=0.17.0", default-features = false, features = ["default-hasher"] }}
stats_alloc = "=0.1.10"
''')
        for profile in ["debug", "release"]:
            command = ["cargo", "build", "--offline", "--manifest-path", str(project / "Cargo.toml"), "--target-dir", str(tmp / "target")]
            if profile == "release":
                command.append("--release")
            build = subprocess.run(command, capture_output=True, text=True)
            log.write(f"BUILD version={version} profile={profile} exit={build.returncode}\n" + build.stderr)
            log.flush()
            build.check_returncode()
            binary = tmp / "target" / profile / f"tree-depth-{version}"
            cases = [(op, depth, stack, 0, 1) for p, op, depth, stack in probes if p == profile]
            if version == "current":
                cases += [(op, 128, stack, 128, 1) for op in ["find", "find_mut"] for stack in [65536, 131072]]
                # Capacity one exercises the largest repetition factor, while
                # full-depth storage exercises the linear path on the same tree.
                if profile == "release":
                    cases += [(op, d, 65536, c, 100) for op in ["find", "find_mut"] for d in [128, 512, 2048] for c in [1, 32, 128, d]]
            for op, depth, stack, capacity, samples in cases:
                result = subprocess.run([str(binary), op, str(depth), str(stack), str(capacity), str(samples)], capture_output=True, text=True, preexec_fn=no_core)
                label = f"version={version} profile={profile} op={op} depth={depth} stack={stack} capacity={capacity} exit={result.returncode}"
                log.write(label + "\n" + result.stdout + result.stderr)
                log.flush()
                print(label, flush=True)
                if version in ["old", "current"]:
                    if result.returncode != 0 or (version == "current" and "allocations=0 reallocations=0" not in result.stdout):
                        raise RuntimeError(f"lookup probe failed: {label}")
    log.write(f"Generated independent probe workspace: {tmp}\n")
print(f"Evidence: {args.output}", flush=True)

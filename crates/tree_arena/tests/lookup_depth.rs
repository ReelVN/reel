// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Lookup stack/allocation contract, independently of build and destruction.
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, hint::black_box, process::Command, time::Instant};
use tree_arena::{ArenaMutList, TreeArena};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn build(mut children: ArenaMutList<'_, u64>, id: u64, depth: u64) {
    let node = children.insert(id, id);
    if id < depth {
        build(node.children, id + 1, depth);
    }
}

#[test]
fn lookup_in_small_stacks() {
    let probes = if cfg!(debug_assertions) {
        vec![
            ("find", 256, 65536),
            ("find_mut", 512, 65536),
            ("find", 512, 131072),
            ("find_mut", 1024, 131072),
        ]
    } else {
        vec![("find", 1024, 65536), ("find_mut", 2048, 65536)]
    };
    let mut cases: Vec<_> = probes
        .into_iter()
        .map(|(op, depth, stack)| (op, depth, stack, 0, 1))
        .collect();
    for op in ["find", "find_mut"] {
        for stack in [65536, 131072] {
            cases.push((op, 128, stack, 128, 1));
        }
        if !cfg!(debug_assertions) {
            for depth in [128, 512, 2048] {
                for capacity in [1, 32, 128, depth] {
                    cases.push((op, depth, 65536, capacity, 100));
                }
            }
        }
    }
    for (op, depth, stack, capacity, samples) in cases {
        let case = format!("{op},{depth},{stack},{capacity},{samples}");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lookup_child", "--nocapture", "--test-threads=1"])
            .env("REEL_LOOKUP_CASE", &case)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let text = String::from_utf8(child.stdout).unwrap();
        assert!(
            text.contains("allocations=0 reallocations=0"),
            "{case}: {text}"
        );
        print!("{text}");
    }
}

#[test]
fn lookup_child() {
    let Ok(case) = std::env::var("REEL_LOOKUP_CASE") else {
        return;
    };
    let args: Vec<_> = case.split(',').collect();
    let op = args[0].to_owned();
    let depth: u64 = args[1].parse().unwrap();
    let stack: usize = args[2].parse().unwrap();
    let capacity: usize = args[3].parse().unwrap();
    let samples: usize = args[4].parse().unwrap();
    let mut tree = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let mut tree = TreeArena::new();
            build(tree.roots_mut(), 1, depth);
            tree
        })
        .unwrap()
        .join()
        .unwrap();
    if capacity > 0 {
        tree.configure_lookup_path_storage(capacity);
    }
    std::thread::Builder::new().name("lookup-probe".into()).stack_size(stack).spawn(move || {
        let mut elapsed = Vec::with_capacity(samples);
        let region = Region::new(ALLOCATOR);
        for _ in 0..samples {
            let start = Instant::now();
            if op == "find" { assert_eq!(*black_box(tree.find(depth).unwrap().item),depth,"lookup identity"); }
            else { *black_box(tree.find_mut(depth).unwrap().item) += 1; }
            elapsed.push(start.elapsed().as_nanos());
        }
        let stats = region.change();
        assert_eq!(stats.allocations,0,"lookup allocated");
        assert_eq!(stats.reallocations,0,"lookup reallocated");
        assert_eq!(*tree.find(depth).unwrap().item,if op == "find" { depth } else { depth+u64::try_from(samples).unwrap() },"mutation result");
        elapsed.sort_unstable();
        println!("OK op={op} depth={depth} stack_bytes={stack} capacity={capacity} samples={samples} allocations={} reallocations={} bytes={} p95_ns={} max_ns={}",stats.allocations,stats.reallocations,stats.bytes_allocated,elapsed[(samples*95).div_ceil(100)-1],elapsed[samples-1]);
        // Recursive nested HashMap destruction is outside the lookup contract.
        // The isolated process releases this tree after reporting the measurement.
        std::mem::forget(tree);
    }).unwrap().join().unwrap();
}

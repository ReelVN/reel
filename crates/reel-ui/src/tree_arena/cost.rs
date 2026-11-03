use super::TreeArena;

#[test]
#[ignore = "release-mode widget lookup measurement"]
fn widget_lookup_cost() {
    for depth in [16_u64, 64, 256] {
        let mut tree = TreeArena::new();
        tree.roots_mut().insert(1_u64, 1_u64);
        for id in 2..=depth {
            tree.find_mut(id - 1).unwrap().children.insert(id, id);
        }
        let start = std::time::Instant::now();
        let mut total = 0;
        for iteration in 0..10_000_u64 {
            let id = 1 + iteration % depth;
            total += *std::hint::black_box(tree.find(id).unwrap().item);
            *tree.find_mut(id).unwrap().item = id;
        }
        eprintln!(
            "widget_lookup depth={depth} lookups=20000 elapsed_us={} checksum={total}",
            start.elapsed().as_micros()
        );
    }
}

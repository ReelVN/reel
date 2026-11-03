use super::*;

fn candidate(
    start: usize,
    end: usize,
    release: usize,
    owner: usize,
    bytes: u64,
    coverage: usize,
) -> Candidate {
    let raster = Raster::new(Affine::IDENTITY, [32; 2], [32; 2], Scissor::full([32; 2]));
    let key = Key {
        drawing: 1,
        push: start,
        raster,
    };
    Candidate {
        entry: Entry {
            root: key,
            input: key,
            resume: coverage,
            commands: 0..coverage,
            token: Token(owner + 1, owner + 1),
            grid: Grid {
                source_transform: Affine::IDENTITY.as_coeffs().map(f64::to_bits),
                effect_transform: Affine::IDENTITY.as_coeffs().map(f64::to_bits),
                effect_context: None,
                source_size: [32; 2],
                source_storage: [32; 2],
                source_scissor: [0, 0, 32, 32],
                output_size: [32; 2],
                output_storage: [32; 2],
                scissor: [0, 0, 32, 32],
                quality: [0, 1],
            },
        },
        owner: Hit {
            token: Token(owner + 1, owner + 1),
            owner,
            bytes,
        },
        events: start..end,
        release,
    }
}

#[test]
fn an_unadmitted_cold_plan_with_no_remaining_owner_returns_an_empty_choice() {
    let candidates = [candidate(0, 5, 6, 0, 20, 10)];
    let mut stats = Stats::default();
    let mut visits = Vec::new();
    let selected = choose(
        &candidates,
        50,
        &mut stats,
        |selected| {
            visits.push(selected.to_vec());
            Ok(80)
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(selected.is_empty());
    assert_eq!(visits, [vec![0], vec![]]);
    assert_eq!(stats.rejected_owners, 1);
}

#[test]
fn owner_budget_keeps_shared_views_together_and_recovers_nested_alternatives() {
    let candidates = [
        candidate(0, 10, 11, 0, 100, 20),
        candidate(0, 6, 7, 1, 20, 12),
        candidate(12, 16, 17, 1, 20, 9),
    ];
    let mut stats = Stats::default();
    let chosen = choose(&candidates, 70, &mut stats, |_| Ok(50), |_| Ok(())).unwrap();
    assert_eq!(chosen, [1, 2]);
    assert_eq!(stats.rejected_owners, 1);
    assert_eq!(stats.recounts, 2);
}

#[test]
fn rejecting_an_atlas_owner_removes_all_of_its_views_before_the_next_recount() {
    let candidates = [
        candidate(0, 4, 5, 0, 80, 2),
        candidate(6, 10, 11, 0, 80, 2),
        candidate(12, 16, 17, 1, 20, 12),
    ];
    let mut stats = Stats::default();
    let mut calls = Vec::new();
    let chosen = choose(
        &candidates,
        50,
        &mut stats,
        |selected| {
            calls.push(selected.to_vec());
            Ok(20)
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(calls, [vec![0, 1, 2], vec![2]]);
    assert_eq!(chosen, [2]);
}

#[test]
fn prefix_exclusions_keep_the_consumers_new_output_acquisition() {
    let candidates = [
        candidate(0, 5, 6, 0, 20, 10),
        candidate(8, 11, 12, 1, 20, 10),
    ];
    let mut skipped = Vec::with_capacity(4);
    exclusions(&candidates, &[0, 1], &mut skipped);
    let remaining: Vec<_> = (0..14)
        .filter(|i| !skipped.iter().any(|range| range.contains(i)))
        .collect();
    assert_eq!(remaining, [5, 7, 11, 13]);
    let mut stats = Stats::default();
    let chosen = choose(&candidates, 30, &mut stats, |_| Ok(30), |_| Ok(())).unwrap();
    assert!(
        chosen.is_empty(),
        "the original admitted plan remains the final fallback"
    );
    assert!(stats.recounts <= candidates.len() as u64 + 1);
}

#[test]
fn deferred_sibling_producers_can_share_a_window_with_out_of_order_releases() {
    let candidates = [
        candidate(0, 3, 20, 0, 10, 8),
        candidate(4, 7, 21, 1, 10, 8),
        candidate(8, 11, 19, 2, 10, 8),
    ];
    let chosen = choose(
        &candidates,
        100,
        &mut Stats::default(),
        |_| Ok(20),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(chosen, [0, 1, 2]);
    let mut skipped = Vec::new();
    exclusions(&candidates, &chosen, &mut skipped);
    let remaining: Vec<_> = (0..23)
        .filter(|index| !skipped.iter().any(|range| range.contains(index)))
        .collect();
    assert_eq!(remaining, [3, 7, 11, 12, 13, 14, 15, 16, 17, 18, 22]);
}

#[test]
fn deferred_release_conflicts_check_every_selected_input() {
    let candidates = [
        candidate(0, 2, 12, 0, 10, 8),
        candidate(3, 5, 15, 1, 10, 8),
        // This retirement conflicts with the first selected input, not the last.
        candidate(6, 8, 12, 2, 10, 8),
        // So does this producer, which includes the first input's retirement.
        candidate(9, 13, 17, 3, 10, 8),
    ];
    let chosen = choose(
        &candidates,
        100,
        &mut Stats::default(),
        |_| Ok(20),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(chosen, [0, 1]);
}

#[test]
fn deferred_siblings_still_reject_overlapping_ancestor_prefixes() {
    let candidates = [
        candidate(0, 10, 20, 0, 10, 12),
        candidate(0, 5, 18, 1, 10, 6),
        candidate(3, 7, 22, 2, 10, 6),
        candidate(11, 14, 21, 3, 10, 8),
    ];
    let chosen = choose(
        &candidates,
        100,
        &mut Stats::default(),
        |_| Ok(20),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(chosen, [0, 3]);
    assert!(
        exclusions_overlap(&(4..7), 15, &(0..2), 6),
        "a macro retirement inside an excluded producer cannot be retained"
    );
}

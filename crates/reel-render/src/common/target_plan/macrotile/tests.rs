use super::*;

fn event(size: [u32; 2], acquired: bool) -> Event {
    Event { size, acquired }
}
fn candidate(root: usize, logical: [u32; 2], count: usize, events: Range<usize>) -> Candidate {
    let (cell, grid) = effect_chain::layout(logical, count + 1).unwrap();
    Candidate {
        owner: 0,
        last_use: true,
        first_cell: 0,
        owner_cells: count + 1,
        key: Key {
            drawing: 1,
            commands: 2,
            length: 100,
            root,
        },
        effects: [0; 3],
        count,
        logical,
        storage: logical,
        cell,
        grid,
        release: events.end - 1,
        events: events.start..events.end - 1,
        window: None,
    }
}
fn bytes(size: [u32; 2]) -> Result<u64, GpuError> {
    Ok(u64::from(size[0]) * u64::from(size[1]) * 4)
}

#[test]
fn deferred_producers_pack_twelve_cells_without_erasing_intervening_lifetimes() {
    let parent = [640, 480];
    let source = [128, 64];
    let other = [27, 19];
    let mut events = vec![event(parent, true)];
    let mut candidates = Vec::new();
    for index in 0..3 {
        let begin = events.len();
        events.push(event(source, true));
        for _ in 0..3 {
            events.extend([event(source, true), event(source, false)]);
        }
        let end = events.len();
        let mut value = candidate(index * 10, source, 3, begin..end + 1);
        value.window = Some((0, index));
        candidates.push(value);
        // This independent owner lives across the deferred source releases.
        if index == 0 {
            events.push(event(other, true));
        }
    }
    for value in &mut candidates {
        value.release = events.len();
        events.push(event(source, false));
    }
    events.push(event(other, false));
    let selected = Selection::choose(
        &events,
        candidates.clone(),
        |s| s,
        &bytes,
        0,
        u64::MAX,
        4096,
        &mut |_| Ok(()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        selected
            .entries
            .iter()
            .map(|entry| entry.owner)
            .collect::<Vec<_>>(),
        [0, 0, 0]
    );
    assert_eq!(
        selected
            .entries
            .iter()
            .map(|entry| entry.first_cell)
            .collect::<Vec<_>>(),
        [0, 4, 8]
    );
    assert!(
        selected
            .entries
            .iter()
            .all(|entry| entry.owner_cells == 12 && entry.last_use)
    );
    assert_eq!(selected.requirements.count(source), 0);
    assert_eq!(selected.requirements.count(other), 1);
    let expected = bytes(parent).unwrap()
        + bytes(other).unwrap()
        + bytes(selected.entries[0].extent()).unwrap();
    assert_eq!(selected.bytes, expected);
    // Reducing the target cap preserves every unselected ordinary source.
    let tight = Selection::choose(
        &events,
        candidates,
        |s| s,
        &bytes,
        0,
        expected - 1,
        4096,
        &mut |_| Ok(()),
    )
    .unwrap()
    .unwrap();
    assert!(tight.bytes < expected);
    assert!(tight.entries.len() < 3);
    assert!(tight.requirements.count(source) != 0);
    assert_eq!(tight.requirements.count(other), 1);
}

#[test]
fn chosen_spans_remove_only_their_ordinary_lifetimes() {
    let root = [640, 480];
    let outside = [80, 40];
    let a = [137, 93];
    let b = [65, 37];
    let mut events = vec![event(root, true), event(outside, true), event(a, true)];
    for _ in 0..3 {
        events.extend([event(a, true), event(a, false)]);
    }
    events.extend([
        event(a, false),
        event(outside, false),
        event(b, true),
        event(b, true),
        event(b, false),
        event(b, false),
    ]);
    let ordinary = remaining(&events, std::iter::empty(), |size| size, &mut |_| Ok(())).unwrap();
    let candidates = vec![candidate(0, a, 3, 2..10), candidate(20, b, 1, 11..15)];
    let atlas_bytes =
        bytes(candidates[0].extent()).unwrap() + bytes(candidates[1].extent()).unwrap();
    let before = cost(&ordinary, &bytes).unwrap() + atlas_bytes;
    let selected = Selection::choose(
        &events,
        candidates,
        |size| size,
        &bytes,
        0,
        u64::MAX,
        4096,
        &mut |_| Ok(()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.entries.len(), 2);
    assert_eq!(selected.requirements.count(root), 1);
    assert_eq!(selected.requirements.count(outside), 1);
    assert_eq!(selected.requirements.count(a), 0);
    assert_eq!(selected.requirements.count(b), 0);
    assert_eq!(
        before - selected.bytes,
        2 * bytes(a).unwrap() + 2 * bytes(b).unwrap()
    );
    assert_eq!(
        selected.bytes,
        bytes(root).unwrap() + bytes(outside).unwrap() + atlas_bytes
    );
}

#[test]
fn an_unaffordable_atlas_keeps_its_complete_ordinary_span() {
    let root = [640, 480];
    let source = [137, 93];
    let events = [
        event(root, true),
        event(source, true),
        event(source, true),
        event(source, false),
        event(source, false),
    ];
    let ordinary = remaining(&events, std::iter::empty(), |size| size, &mut |_| Ok(())).unwrap();
    let limit = cost(&ordinary, &bytes).unwrap();
    assert!(
        Selection::choose(
            &events,
            vec![candidate(0, source, 1, 1..5)],
            |size| size,
            &bytes,
            0,
            limit,
            4096,
            &mut |_| Ok(())
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(ordinary.count(source), 2);
}

#[test]
fn failed_preparation_releases_new_owners_and_tickets_before_ordinary_acquisition() {
    use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
    use std::rc::Rc;
    struct Owner {
        _physical: BudgetTicket,
    }
    struct State {
        physical: BudgetPool,
        local: BudgetPool,
        cache: Vec<Rc<Owner>>,
        staged: Vec<Rc<Owner>>,
        targets: Vec<Rc<Owner>>,
        tickets: Vec<BudgetTicket>,
    }
    let pool = || {
        BudgetPool::new(
            BudgetCategory::Offscreen,
            QueueLimits {
                max_requests: 32,
                max_bytes: 100,
            },
        )
    };
    let mut state = State {
        physical: pool(),
        local: pool(),
        cache: vec![],
        staged: vec![],
        targets: vec![],
        tickets: vec![],
    };
    state.cache.push(Rc::new(Owner {
        _physical: state.physical.reserve(30).unwrap(),
    }));
    let value = prepare_or_ordinary(
        &mut state,
        |_| true,
        |state| {
            let atlas = Rc::new(Owner {
                _physical: state.physical.reserve(40).unwrap(),
            });
            state.staged.push(atlas.clone());
            state.targets.extend([atlas, state.cache[0].clone()]);
            state.tickets.push(state.local.reserve(70).unwrap());
            assert_eq!(state.physical.usage().bytes, 70);
            Err(GpuError::Pressure {
                domain: "offscreen targets",
                requested: 40,
                available: 30,
            })
        },
        |state| {
            drop(std::mem::take(&mut state.staged));
            drop(std::mem::take(&mut state.targets));
            state.tickets.clear();
            Ok(())
        },
        |state| {
            assert_eq!(
                state.physical.usage().bytes,
                30,
                "only the preexisting cache owner remains"
            );
            assert_eq!(state.local.usage().bytes, 0);
            assert_eq!(
                state.cache.len(),
                1,
                "failed new owners were never published"
            );
            let ticket = state.physical.reserve(70).unwrap();
            assert_eq!(state.physical.usage().bytes, 100);
            drop(ticket);
            Ok(7)
        },
    )
    .unwrap();
    assert_eq!(value, 7);
    assert_eq!(Rc::strong_count(&state.cache[0]), 1);
}

#[test]
fn a_release_fault_aborts_before_ordinary_preparation() {
    let mut ordinary_called = false;
    let result = prepare_or_ordinary(
        &mut ordinary_called,
        |_| true,
        |_| Err::<(), _>(GpuError::InvalidSize),
        |_| Err(GpuError::Device("release fault".into())),
        |called| {
            *called = true;
            Ok(())
        },
    );
    assert!(matches!(result, Err(GpuError::Device(_))));
    assert!(!ordinary_called);
}

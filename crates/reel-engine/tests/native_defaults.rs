use reel_engine::aside::source;
use std::collections::BTreeSet;

#[test]
fn registered_literal_defaults_measure_the_real_seed() {
    let contracts = source::contracts();
    let capabilities = contracts
        .iter()
        .flat_map(|contract| &contract.required_capabilities)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let seed = aside_host::seed(contracts, source::types(), &capabilities, 1).unwrap();
    let declared = contracts
        .iter()
        .map(|contract| {
            contract
                .parameters
                .iter()
                .filter(|parameter| parameter.default.is_some())
                .count()
        })
        .sum::<usize>();
    let helpers = seed.package().functions.len() - contracts.len();
    let bytes = aside_vm::bytecode::encode(seed.package(), Default::default())
        .unwrap()
        .len();
    eprintln!(
        "registered native defaults: declarations={declared}, helpers={helpers}, native_functions={}, functions={}, signatures={}, constants={}, symbols={}, encoded_bytes={bytes}, owned_bytes={}",
        contracts.len(),
        seed.package().functions.len(),
        seed.package().signatures.len(),
        seed.package().constants.len(),
        seed.package().symbols.len(),
        seed.stats().owned_capacity_bytes
    );
    assert!(
        helpers < declared,
        "the registered node schemas must share literal defaults"
    );
}

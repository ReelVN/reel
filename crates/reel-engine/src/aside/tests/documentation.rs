use super::*;

#[test]
fn source_api_examples_run_with_the_real_reel_registry() {
    let missing: Vec<_> = source::documentation()
        .into_iter()
        .filter(|entry| entry.markdown.trim().is_empty())
        .map(|entry| entry.contract.symbol)
        .collect();
    assert!(
        missing.is_empty(),
        "registered APIs missing source documentation: {missing:?}"
    );
    for entry in source::documentation() {
        for example in aside_compiler::documentation::examples(entry.markdown) {
            let code = format!("{}\nfn opening() {{ main() }}", example.source);
            for encoded in [false, true] {
                let (mut session, mut vm) = models::prepared(&code, encoded);
                models::drive(&mut vm, &mut session, false);
                assert!(
                    vm.task().is_none(),
                    "{}:{} did not finish",
                    entry.contract.symbol,
                    example.line
                );
            }
        }
    }
    for entry in source::node_property_documentation() {
        assert!(
            !entry.markdown.trim().is_empty(),
            "{} has no hover documentation",
            entry.property.name
        );
    }
}

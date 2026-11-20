use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::NodeKind;

#[test]
fn explicit_selection_predicts_a_capturing_factory_until_it_settles() {
    let code = r#"fn opening() {
        let content = model({caption: "Selected page"});
        let pages = page_stack(value: "one", items: [
            {key: "one", build: || text("First page")},
            {key: "two", build: || stack {
                image("requested.png", width: 100, height: 100);
                text(content.caption);
            }},
        ]);
        stage.replace(column {
            pages;
            button("Open", on_click: || { await pages.select("two"); });
        });
    }"#;
    let (mut session, mut vm) = prepared(code, false);
    vm.visual_plan =
        visual_plan::VisualPlan::compile(vm.vm.program(), 64 * 1024).map(std::sync::Arc::new);
    drive(&mut vm, &mut session, true);
    let button = vm
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Button {
                label: Some(label), ..
            } if label == "Open" => Some(node.id),
            _ => None,
        })
        .unwrap();
    assert!(vm.predicted_images().is_empty());
    assert!(vm.click(&mut session, button).unwrap());
    for _ in 0..16 {
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut session) else {
            panic!("page selection settled before publishing a prediction")
        };
        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
            .unwrap();
        if !vm.predicted_images().is_empty() {
            break;
        }
    }
    assert_eq!(vm.predicted_images(), &[Box::<str>::from("requested.png")]);
    assert!(vm.stage().nodes().all(|node| !matches!(
        &node.kind, NodeKind::Text(label) if label.as_ref() == "Selected page"
    )));
    drive(&mut vm, &mut session, true);
    assert!(vm.predicted_images().is_empty());
    assert!(vm.stage().nodes().any(|node| matches!(
        &node.kind, NodeKind::Text(label) if label.as_ref() == "Selected page"
    )));
}

use super::*;
use crate::localization::{Catalog, CatalogLoader};
use reel_ui::stage::{NodeId, NodeKind};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

#[derive(Default)]
struct Loader {
    next: u64,
    pending: HashMap<u64, Result<Catalog, String>>,
    cancelled: Rc<RefCell<Vec<u64>>>,
}
impl CatalogLoader for Loader {
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String> {
        self.next += 1;
        let text = match source {
            "valid.ftl" => {
                "hello = Hello\ncount = { $n ->\n [one] One\n *[other] { $n } messages\n }\nname = Hello, { $name }!\n"
            }
            "broken.ftl" => "hello = {\n",
            "pending.ftl" => return Ok(self.next),
            _ => return Err("missing catalog".into()),
        };
        self.pending
            .insert(self.next, Catalog::parse(locale, source, text.into()));
        Ok(self.next)
    }
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>> {
        self.pending.remove(&id)
    }
    fn cancel(&mut self, id: u64) {
        self.cancelled.borrow_mut().push(id);
        self.pending.remove(&id);
    }
}
fn click(vm: &mut SessionVm, session: &mut Session, label: &str) -> NodeId {
    let id = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Button { label: Some(s), .. } if s == label))
        .unwrap()
        .id;
    assert!(vm.click(session, id).unwrap());
    models::drive(vm, session, true);
    id
}
#[test]
fn external_catalogs_react_without_changing_committed_interactions() {
    let code = r#"fn opening() {
        localization.set_locale("zh-CN");
        let data = model({n: 1});
        let caption = text(value: derive(|| tr("你好", id: "hello")));
        let bilingual = column {
            text(tr("你好", id: "hello", locale: "zh-CN"));
            text(value: derive(|| tr("你好", id: "hello", locale: "en-US")));
        };
        stage.replace(column() {
            caption;
            bilingual;
            text(value: derive(|| tr("{ $n } 条消息", id: "count", params: {n: data.n})));
            button("English", on_click: |event| {
                localization.set_locale("en-US"); data.n = 2;
            });
            button("Chinese", on_click: |event| localization.set_locale("zh-CN"));
        });
        localization.load("en", "valid.ftl");
        if localization.locale() != "zh-CN"
           || tr("你好", id: "hello", locale: "en-US") != "Hello"
           || tr("原文", id: "missing", locale: "en") != "原文"
           || tr("", id: "count", params: {n: 1}, locale: "en") != "One" {
            throw "per-call language changed global selection or lookup";
        }
        let mut invalid_locale = false;
        try { tr("你好", locale: "not a locale"); } catch error { invalid_locale = true; }
        if !invalid_locale || localization.locale() != "zh-CN" { throw "locale validation"; }
        let mut rejected = false;
        try { localization.load("en", "broken.ftl"); } catch error {
            rejected = error.starts_with("broken.ftl:");
        }
        if !rejected { throw "missing source diagnostic"; }
        narrator(tr("你好", id: "hello"));
        localization.set_locale("en-US");
        if tr("你好", id: "hello") != "Hello" { throw "atomic catalog replacement/base fallback"; }
        if tr("literal {0} <~#", id: "missing") != "literal {0} <~#" { throw "literal fallback"; }
        if tr("", id: "count", params: {n: 1}) != "One" { throw "plural"; }
        if !tr("", id: "name", params: {name: "{ $secret }"}).starts_with("Hello, ") { throw "parameter"; }
        let mut missing = false;
        try { tr("", id: "name"); } catch error { missing = true; }
        if !missing { throw "missing parameter silently accepted"; }
        let choice = choose([{key: "stable", label: tr("你好", id: "hello")}]);
        if choice != "stable" { throw "translated key"; }
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        vm.set_catalog_loader(Box::<Loader>::default()).unwrap();
        models::drive(&mut vm, &mut session, true);
        let caption = vm
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("你好".into()))
            .unwrap()
            .id;
        let english = click(&mut vm, &mut session, "English");
        assert_eq!(
            vm.stage().node(caption).unwrap().kind,
            NodeKind::Text("Hello".into())
        );
        assert!(vm.stage().nodes().any(
            |n| matches!(&n.kind, NodeKind::Text(s) if s.contains("2") && s.ends_with(" messages"))
        ));
        let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction else {
            panic!("dialogue")
        };
        assert_eq!(dialogue.text(), "你好");
        assert_eq!(dialogue.page_locale(), "zh-CN");
        let interaction = dialogue.id();
        collect(&mut vm);
        click(&mut vm, &mut session, "Chinese");
        assert_eq!(
            vm.stage().node(caption).unwrap().kind,
            NodeKind::Text("你好".into())
        );
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Hello".into()))
        );
        assert_eq!(click(&mut vm, &mut session, "English"), english);
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, true);
        let Some(crate::InteractionView::Choice(choice)) = session.view().interaction else {
            panic!("choice")
        };
        let option = choice.options()[0].id().clone();
        click(&mut vm, &mut session, "Chinese");
        let Some(crate::InteractionView::Choice(choice)) = session.view().interaction else {
            panic!("choice")
        };
        assert_eq!(choice.options()[0].id(), &option);
        assert_eq!(choice.options()[0].label(), "Hello");
        session
            .handle_local_input(session.root_owner(), InputAction::Select(option))
            .unwrap();
        models::drive(&mut vm, &mut session, true);
    }
}

#[test]
fn cancellation_retires_catalog_request_and_prevents_late_publication() {
    let (mut session, mut vm) = models::prepared(
        "fn opening() { localization.load(\"en\", \"pending.ftl\"); throw \"resumed cancelled load\"; }",
        true,
    );
    let cancelled = Rc::new(RefCell::new(Vec::new()));
    vm.set_catalog_loader(Box::new(Loader {
        cancelled: cancelled.clone(),
        ..Default::default()
    }))
    .unwrap();
    models::drive(&mut vm, &mut session, true);
    assert!(vm.localization_pending());
    assert!(vm.set_catalog_loader(Box::<Loader>::default()).is_err());
    vm.revoke(&mut session).unwrap();
    assert_eq!(&*cancelled.borrow(), &[1]);
    assert!(!vm.localization_pending());
    assert!(!vm.maintain_localization().unwrap());
}

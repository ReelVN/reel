use super::*;
use crate::localization::{Catalog, CatalogLoader};
use reel_ui::stage::{NodeId, NodeKind};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

// External-resource fixtures obtain the actual compiler identities. The author
// calls below use source text; no test reimplements the content hashing scheme.
fn fixture_catalog(text: &str) -> String {
    static KEYS: std::sync::OnceLock<HashMap<&'static str, Vec<String>>> =
        std::sync::OnceLock::new();
    let keys = KEYS.get_or_init(|| {
        let mut keys: HashMap<_, Vec<_>> = HashMap::new();
        for (entry, original, params) in [
            ("hello", "你好", false),
            ("hello", "原文", false),
            ("hello", "source", false),
            ("base", "base source", false),
            ("count", "{ $n } 条消息", true),
            ("name", "Hello, { $name }!", true),
        ] {
            let code = format!(
                "fn fixture() {{ tr({original:?}{}); }}",
                if params { ", params: {}" } else { "" }
            );
            let compiled =
                source::compile_module(&code, "catalog_fixture", "catalog_fixture", 1).unwrap();
            keys.entry(entry)
                .or_default()
                .push(compiled.catalogs.messages[0].key.clone());
        }
        keys
    });
    let mut result = String::new();
    let mut current = None;
    let mut body = String::new();
    let flush = |current: Option<&str>, body: &str, result: &mut String| {
        if let Some(current) = current {
            if let Some(aliases) = keys.get(current) {
                for key in aliases {
                    result.push_str(key);
                    result.push_str(" =");
                    result.push_str(body);
                }
            } else {
                result.push_str(current);
                result.push_str(" =");
                result.push_str(body);
            }
        }
    };
    for line in text.split_inclusive('\n') {
        if !line.starts_with(char::is_whitespace)
            && let Some((name, value)) = line.split_once(" =")
        {
            flush(current, &body, &mut result);
            current = Some(name);
            body.clear();
            body.push_str(value);
        } else {
            body.push_str(line);
        }
    }
    flush(current, &body, &mut result);
    result
}

#[derive(Default)]
struct Loader {
    next: u64,
    pending: HashMap<u64, Result<Catalog, String>>,
    cancelled: Rc<RefCell<Vec<u64>>>,
}
impl CatalogLoader for Loader {
    fn resolve(&self, _locale: &str) -> Option<&str> {
        None
    }
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
        self.pending.insert(
            self.next,
            Catalog::parse(locale, source, fixture_catalog(text)),
        );
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
        let caption = text(value: derive(|| tr("你好")));
        let bilingual = column {
            text(tr("你好", locale: "zh-CN"));
            text(value: derive(|| tr("你好", locale: "en-US")));
        };
        stage.replace(column() {
            caption;
            bilingual;
            text(value: derive(|| tr("{ $n } 条消息", params: {n: data.n})));
            button("English", on_click: |event| {
                localization.set_locale("en-US"); data.n = 2;
            });
            button("Chinese", on_click: |event| localization.set_locale("zh-CN"));
        });
        localization.load("en", "valid.ftl");
        if localization.locale() != "zh-CN"
           || tr("你好", locale: "en-US") != "Hello"
           || tr("未翻译原文", locale: "en") != "未翻译原文"
           || tr("{ $n } 条消息", params: {n: 1}, locale: "en") != "One" {
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
        narrator(tr("你好"));
        localization.set_locale("en-US");
        if tr("你好") != "Hello" { throw "atomic catalog replacement/base fallback"; }
        if tr("literal {0} <~#") != "literal {0} <~#" { throw "literal fallback"; }
        if tr("{ $n } 条消息", params: {n: 1}) != "One" { throw "plural"; }
        if !tr("Hello, { $name }!", params: {name: "{ $secret }"}).starts_with("Hello, ") { throw "parameter"; }
        let mut missing = false;
        try { tr("Hello, { $name }!", params: {}); } catch error { missing = true; }
        if !missing { throw "missing parameter silently accepted"; }
        let choice = choose([{key: "stable", label: tr("你好")}]);
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

#[derive(Default)]
struct CatalogRequests {
    started: Vec<(u64, String, String)>,
    cancelled: Vec<u64>,
    ready: HashMap<u64, Result<Catalog, String>>,
    text: HashMap<String, String>,
    held: HashSet<String>,
}
impl CatalogRequests {
    fn release(&mut self, source: &str) {
        self.held.remove(source);
        for (id, locale, key) in &self.started {
            if key == source {
                self.ready.insert(
                    *id,
                    Catalog::parse(locale, source, self.text[source].clone()),
                );
            }
        }
    }
    fn loads(&self, locale: &str) -> usize {
        self.started
            .iter()
            .filter(|(_, language, _)| language == locale)
            .count()
    }
}
struct IndexedLoader {
    index: HashMap<String, String>,
    requests: Rc<RefCell<CatalogRequests>>,
}
impl CatalogLoader for IndexedLoader {
    fn resolve(&self, locale: &str) -> Option<&str> {
        self.index.get(locale).map(String::as_str)
    }
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String> {
        let mut requests = self.requests.borrow_mut();
        let text = requests
            .text
            .get(source)
            .ok_or_else(|| format!("missing catalog {source}"))?
            .clone();
        let id = requests.started.len() as u64 + 1;
        requests.started.push((id, locale.into(), source.into()));
        if !requests.held.contains(source) {
            requests
                .ready
                .insert(id, Catalog::parse(locale, source, text));
        }
        Ok(id)
    }
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>> {
        self.requests.borrow_mut().ready.remove(&id)
    }
    fn cancel(&mut self, id: u64) {
        let mut requests = self.requests.borrow_mut();
        requests.cancelled.push(id);
        requests.ready.remove(&id);
    }
}
fn indexed_loader(held: &[&str]) -> (Box<IndexedLoader>, Rc<RefCell<CatalogRequests>>) {
    let entries = [
        ("en", "en.ftl", "hello = Hello\nbase = Base\n"),
        ("en-US", "us.ftl", "hello = Howdy\n"),
        ("fr", "fr.ftl", "hello = Bonjour\nbase = Français\n"),
        ("fr-FR", "france.ftl", "hello = Salut\n"),
        ("de", "de.ftl", "hello = Hallo\n"),
        ("es", "es.ftl", "hello = Hola\n"),
        ("es-MX", "broken.ftl", "hello = {\n"),
    ];
    let index = entries
        .iter()
        .map(|(locale, source, _)| (locale.to_string(), source.to_string()))
        .collect();
    let requests = Rc::new(RefCell::new(CatalogRequests {
        text: entries
            .iter()
            .map(|(_, source, text)| (source.to_string(), fixture_catalog(text)))
            .collect(),
        held: held.iter().map(|source| source.to_string()).collect(),
        ..Default::default()
    }));
    (
        Box::new(IndexedLoader {
            index,
            requests: requests.clone(),
        }),
        requests,
    )
}
fn has_caption(vm: &SessionVm, caption: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|node| matches!(&node.kind, NodeKind::Text(text) if &**text == caption))
}
fn restore_catalogs(
    vm: &mut SessionVm,
    session: &Session,
    loader: Box<dyn CatalogLoader>,
) -> (Session, SessionVm) {
    use aside_vm::snapshot::SnapshotBudget;
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(20),
        },
        crate::ReadProgress::default(),
        None,
        Some(loader),
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn indexed_selection_commits_exact_and_base_together_and_releases_only_automatic_catalogs() {
    let code = r#"fn opening() {
        localization.set_locale("zh-CN");
        localization.load("de");
        stage.replace(column {
            text(value: derive(|| tr("原文")));
            button("English", on_click: || {
                localization.set_locale("en-US");
                if tr("source") != "Howdy" || tr("base source") != "Base" {
                    throw "exact/base selection";
                }
            });
            button("French", on_click: || {
                localization.set_locale("fr");
                if tr("source", locale:"en-US") != "source" {
                    throw "automatic catalog survived";
                }
                if tr("source", locale:"de") != "Hallo" {
                    throw "explicit preload was released";
                }
            });
            button("Pin French", on_click: || localization.load("fr"));
            button("Check pinned", on_click: || {
                if tr("source",locale:"fr") != "Bonjour" {
                    throw "resident preload was not retained";
                }
            });
        });
        narrator("committed");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (loader, requests) = indexed_loader(&["us.ftl", "en.ftl"]);
        vm.set_catalog_loader(loader).unwrap();
        models::drive(&mut vm, &mut session, true);
        assert_eq!(
            requests.borrow().started.len(),
            1,
            "only explicit German preload"
        );
        assert!(has_caption(&vm, "原文"));
        click(&mut vm, &mut session, "English");
        assert_eq!(vm.locale().as_deref(), Some("zh-CN"));
        requests.borrow_mut().release("us.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("zh-CN"));
        assert!(has_caption(&vm, "原文"), "partial candidate was published");
        requests.borrow_mut().release("en.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("en-US"));
        assert!(has_caption(&vm, "Howdy"));
        let loaded = requests.borrow().started.len();
        click(&mut vm, &mut session, "English");
        assert_eq!(
            requests.borrow().started.len(),
            loaded,
            "resident selection reread assets"
        );
        click(&mut vm, &mut session, "French");
        assert!(has_caption(&vm, "Bonjour"));
        let loaded = requests.borrow().started.len();
        click(&mut vm, &mut session, "Pin French");
        assert_eq!(requests.borrow().started.len(), loaded);
        click(&mut vm, &mut session, "English");
        click(&mut vm, &mut session, "Check pinned");
        assert_eq!(requests.borrow().loads("en-US"), 2);
        assert_eq!(requests.borrow().loads("en"), 2);
        assert_eq!(requests.borrow().loads("de"), 1);
        let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction else {
            panic!("committed dialogue");
        };
        assert_eq!(dialogue.text(), "committed");
        assert_eq!(dialogue.page_locale(), "zh-CN");
    }
}

#[test]
fn failed_indexed_selection_keeps_old_language_and_discards_every_candidate() {
    let code = r#"fn opening() {
        localization.set_locale("en");
        stage.replace(column {
            text(value: derive(|| tr("source")));
            button("Broken", on_click: || {
                let mut failed = false;
                try { localization.set_locale("es-MX"); } catch error {
                    failed = error.starts_with("broken.ftl:");
                }
                if !failed || localization.locale() != "en" || tr("source") != "Hello"
                    || tr("source",locale:"es") != "source" {
                    throw "failed selection changed installed state";
                }
            });
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (loader, requests) = indexed_loader(&["es.ftl"]);
        vm.set_catalog_loader(loader).unwrap();
        models::drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "Broken");
        assert_eq!(vm.locale().as_deref(), Some("en"));
        assert!(has_caption(&vm, "Hello"));
        assert!(!vm.localization_pending());
        assert_eq!(
            requests.borrow().cancelled.len(),
            1,
            "only the still-pending fallback request needs cancellation"
        );
        requests.borrow_mut().release("es.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("en"));
        assert!(has_caption(&vm, "Hello"));
    }
}

#[test]
fn newer_selection_and_callback_retirement_prevent_late_publication() {
    let code = r#"fn opening() {
        localization.set_locale("en");
        let state=model({finished:0});
        stage.replace(column {
            text(value: derive(|| f"finished:{state.finished}"));
            button("Slow", on_click: || {localization.set_locale("fr");state.finished+=1;});
            button("Fast", on_click: || localization.set_locale("de"));
            button("Retire", on_click: || stage.replace(text("retired")));
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (loader, requests) = indexed_loader(&["fr.ftl"]);
        vm.set_catalog_loader(loader).unwrap();
        models::drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "Slow");
        assert_eq!(vm.locale().as_deref(), Some("en"));
        click(&mut vm, &mut session, "Fast");
        assert_eq!(vm.locale().as_deref(), Some("de"));
        assert!(
            has_caption(&vm, "finished:1"),
            "superseded call did not settle normally"
        );
        requests.borrow_mut().release("fr.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("de"));
        assert!(!vm.localization_pending());
        requests.borrow_mut().held.insert("fr.ftl".into());
        click(&mut vm, &mut session, "Slow");
        assert!(vm.localization_pending());
        click(&mut vm, &mut session, "Retire");
        assert!(!vm.localization_pending());
        requests.borrow_mut().release("fr.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("de"));
        assert!(has_caption(&vm, "retired"));
        assert_eq!(requests.borrow().cancelled.len(), 2);
    }
}

#[test]
fn source_only_selection_needs_no_loader_or_requests() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening() {
                localization.set_locale("zh-CN");
                localization.load("zh-CN");
                if localization.locale() != "zh-CN" || tr("原文") != "原文" {
                    throw "source-only locale";
                }
                narrator("ready");
            }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("zh-CN"));
        assert!(!vm.localization_pending());
    }
}

#[test]
fn snapshot_restores_catalog_ownership_and_one_atomic_pending_selection() {
    let code = r#"fn opening() {
        localization.load("de");
        localization.set_locale("en");
        stage.replace(column {
            text(value: derive(|| tr("source")));
            button("French",on_click:|| localization.set_locale("fr-FR"));
            button("English",on_click:|| {
                localization.set_locale("en");
                if tr("source",locale:"de") != "Hallo"
                    || tr("source",locale:"fr-FR") != "source" {
                    throw "restored catalog ownership";
                }
            });
        });
        narrator("save");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (loader, requests) = indexed_loader(&["france.ftl", "fr.ftl"]);
        vm.set_catalog_loader(loader).unwrap();
        models::drive(&mut vm, &mut session, true);
        click(&mut vm, &mut session, "French");
        requests.borrow_mut().release("france.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("en"));
        let (loader, restored_requests) = indexed_loader(&["france.ftl", "fr.ftl"]);
        let (mut session, mut vm) = restore_catalogs(&mut vm, &session, loader);
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("en"));
        assert!(has_caption(&vm, "Hello"));
        restored_requests.borrow_mut().release("france.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("en"));
        restored_requests.borrow_mut().release("fr.ftl");
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.locale().as_deref(), Some("fr-FR"));
        assert!(has_caption(&vm, "Salut"));
        click(&mut vm, &mut session, "English");
        assert_eq!(vm.locale().as_deref(), Some("en"));
        assert_eq!(restored_requests.borrow().loads("en"), 2);
        assert_eq!(restored_requests.borrow().loads("de"), 1);
    }
}

#[test]
fn concurrent_preload_keeps_its_reused_base_until_success_failure_or_cancellation() {
    let code = r#"fn opening() {
        localization.set_locale("en");
        let state=model({status:"waiting"});
        stage.replace(column {
            text(value:derive(||tr("base source",locale:"en-US")));
            text(value:derive(||state.status));
            button("Warm",on_click:|| {
                try { localization.load("en-US"); state.status="done"; }
                catch error { state.status="failed"; }
            });
            button("French",on_click:||localization.set_locale("fr"));
            button("Japanese",on_click:||localization.set_locale("ja"));
            button("Retire",on_click:||stage.replace(
                text(value:derive(||tr("base source",locale:"en-US")))
            ));
        });
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        for restored in [false, true] {
            for outcome in ["success", "failure", "cancel"] {
                let (mut session, mut vm) = models::prepared(code, encoded);
                let (loader, mut requests) = indexed_loader(&["us.ftl"]);
                vm.set_catalog_loader(loader).unwrap();
                models::drive(&mut vm, &mut session, true);
                click(&mut vm, &mut session, "Warm");
                assert!(vm.localization_pending());
                click(&mut vm, &mut session, "French");
                assert_eq!(vm.locale().as_deref(), Some("fr"));
                assert!(
                    has_caption(&vm, "Base"),
                    "reused base was evicted before preload settled"
                );
                assert_eq!(
                    requests.borrow().loads("en"),
                    1,
                    "preload reread its resident base"
                );
                if restored {
                    let (loader, restored_requests) = indexed_loader(&["us.ftl"]);
                    (session, vm) = restore_catalogs(&mut vm, &session, loader);
                    requests = restored_requests;
                    models::drive(&mut vm, &mut session, true);
                    assert_eq!(vm.locale().as_deref(), Some("fr"));
                    assert!(
                        has_caption(&vm, "Base"),
                        "restored pending retention was lost"
                    );
                    assert_eq!(requests.borrow().loads("en"), 1);
                }
                match outcome {
                    "success" => {
                        requests.borrow_mut().release("us.ftl");
                        models::drive(&mut vm, &mut session, true);
                        assert!(has_caption(&vm, "done"));
                        assert!(has_caption(&vm, "Base"));
                        click(&mut vm, &mut session, "Japanese");
                        assert!(
                            has_caption(&vm, "Base"),
                            "successful preload did not pin its reused base"
                        );
                    }
                    "failure" => {
                        requests
                            .borrow_mut()
                            .text
                            .insert("us.ftl".into(), "hello = {\n".into());
                        requests.borrow_mut().release("us.ftl");
                        models::drive(&mut vm, &mut session, true);
                        assert!(has_caption(&vm, "failed"));
                        assert!(
                            has_caption(&vm, "base source"),
                            "failed preload retained its old base"
                        );
                    }
                    "cancel" => {
                        click(&mut vm, &mut session, "Retire");
                        requests.borrow_mut().release("us.ftl");
                        models::drive(&mut vm, &mut session, true);
                        assert!(
                            has_caption(&vm, "base source"),
                            "cancelled preload retained its old base"
                        );
                    }
                    _ => unreachable!(),
                }
                assert!(!vm.localization_pending());
                assert_eq!(
                    requests.borrow().loads("en"),
                    1,
                    "reused base was read twice"
                );
            }
        }
    }
}

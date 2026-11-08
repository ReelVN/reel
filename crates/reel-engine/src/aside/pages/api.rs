//! Source declarations for the native page and tab compositions.
use super::*;
use aside_vm::bytecode::{BuiltinType, Const};
use registry::{NativeContract, NativeParameter};

pub(super) fn tab_factory_type() -> aside_compiler::semantic::TypeHint {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{named, nullable};
    nullable(TypeHint::Callable(
        vec![named("reel::types::TabItem")],
        Box::new(nullable(named("reel::types::Node"))),
    ))
}
aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Keep lazily constructed pages under a common stage container. `items` is a
        /// fixed array of unique `{key, build}` records; build returns a Node. A non-null value
        /// selects its key; null hides every page. Factories may yield for VM budgets,
        /// but cannot await external work. The previous page stays projected while a
        /// new page is being constructed. Built pages survive selection and visibility
        /// changes until eviction or node retirement. Page visits, audio and previews
        /// use ordinary explicit scopes. Each page's own `visible` binding is preserved.
        ///
        /// ```aside
        /// fn main() {
        /// let selected = model({page: "sound"});
        /// let pages = page_stack(value: bind(selected.page), items: [
        ///     {key: "sound", build: || text("Sound")},
        ///     {key: "display", build: || text("Display")}
        /// ]);
        /// stage.replace(pages);
        /// }
        /// ```
        "reel::page_stack";
        /// Return the cached node for this key, or null before successful construction.
        "reel::page_stack::get";
        /// Return an owned Task that produces the page node. Concurrent calls for one
        /// key share one producer. Await cancellation detaches that consumer; cancelling
        /// the Task or retiring this stack cancels construction. Failed/cancelled builds
        /// settle all consumers; a later explicit ensure may retry. Hidden or detached
        /// ensures add no page-readiness gate; factories retain ordinary Task commit
        /// boundaries. Existing cached pages return their settled Task.
        "reel::page_stack::ensure";
        /// Retire a cached inactive page and return true. Return false if it has not
        /// been built. Evicting the selected key raises a source-located state error.
        "reel::page_stack::evict";
        /// Build one page through the VM's owned no-wait Task continuation.
        "reel::page_stack::produce";
        /// Create independently focusable native tabs over fixed unique `{key,label}`
        /// records. Share value with any page stack or authored view. Each bar has its
        /// own focus group. Horizontal/vertical arrows wrap within the bar; automatic
        /// activation selects on arrows, manual activation selects on confirmation.
        /// The optional no-wait `build: fn(TabItem) -> (Node | null)` returns custom
        /// content. A direct `|item|` lambda inherits this signature during source
        /// checking, including member access and result validation. Its read-only
        /// observable fields are `key`, `label`, `selected`, and `control: RadioNode`.
        /// The control exists before the factory runs. Use its ordinary appearance,
        /// hover and focus_visible properties with bind/derive to style custom content.
        /// The native option owns click, focus and
        /// accessibility, including a focus ring for custom manual tabs. All common
        /// node layout and appearance properties apply.
        ///
        /// ```aside
        /// fn main() {
        /// let selected = model({page: "sound"});
        /// let tabs = tab_bar(value: bind(selected.page), items: [
        ///     {key: "sound", label: "Sound"},
        ///     {key: "display", label: "Display"}
        /// ], build: |item| {
        ///     item.control.appearance = appearance(hover: {opacity: 0.8});
        ///     row(gap: 4) {
        ///         text(bind(item.label), opacity: derive(|| if item.selected { 1 } else { 0.5 }));
        ///         text("Focused", visible: bind(item.control.focus_visible));
        ///     }
        /// });
        /// stage.replace(tabs);
        /// }
        /// ```
        "reel::tab_bar";
    }
}
pub(in crate::aside) fn contracts() -> Vec<NativeContract> {
    let mut contracts: Vec<_> = ["get", "ensure", "evict", "produce"]
        .into_iter()
        .map(|name| NativeContract {
            symbol: format!("reel::page_stack::{name}"),
            abi_version: 1,
            parameters: if name == "produce" {
                vec![NativeParameter {
                    name: "request".into(),
                    ty: BuiltinType::Any,
                    default: None,
                }]
            } else {
                vec![
                    NativeParameter {
                        name: "stack".into(),
                        ty: BuiltinType::Any,
                        default: None,
                    },
                    NativeParameter {
                        name: "key".into(),
                        ty: BuiltinType::Any,
                        default: None,
                    },
                ]
            },
            result: BuiltinType::Any,
            effects: Effects(
                Effects::ALLOCATE | Effects::HOST | if name == "get" { 0 } else { Effects::WRITE },
            ),
            standard_derived_commit: false,
            read_only_leaf: None,
            rest: None,
            required_capabilities: vec!["reel.stage".into()],
        })
        .collect();
    contracts.push(NativeContract {
        symbol: "reel::tab_bar::observe_label".into(),
        abi_version: 1,
        parameters: vec![NativeParameter {
            name: "output".into(),
            ty: BuiltinType::Any,
            default: None,
        }],
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        standard_derived_commit: false,
        read_only_leaf: None,
        rest: None,
        required_capabilities: vec!["reel.stage".into()],
    });
    contracts
}
pub(in crate::aside) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, nullable};
    let stack = "reel::types::PageStackNode";
    for (method, result) in [
        ("get", nullable(named("reel::types::Node"))),
        ("ensure", named("reel::types::Task")),
        ("evict", TypeHint::Bool),
    ] {
        let symbol = format!("reel::page_stack::{method}");
        api.refine(&symbol, result, &[("stack", named(stack))]);
        let member = Member::method(api.native(&symbol).clone().bound(method));
        api.types
            .iter_mut()
            .find(|ty| ty.name == stack)
            .unwrap()
            .members
            .push(member);
    }
    api.types.push(NamedType::new(
        "reel::types::TabItem",
        "Read-only observable tab content data.",
        vec![
            Member::field(
                "reel::types::TabItem",
                "key",
                TypeHint::Any,
                None,
                "Stable tab key.",
                None,
            ),
            Member::field(
                "reel::types::TabItem",
                "label",
                nullable(TypeHint::String),
                None,
                "Resolved observable label.",
                Some(Const::Null),
            ),
            Member::field(
                "reel::types::TabItem",
                "selected",
                TypeHint::Bool,
                None,
                "Whether this key is selected.",
                Some(Const::Bool(false)),
            ),
            Member::field(
                "reel::types::TabItem",
                "control",
                named("reel::types::RadioNode"),
                None,
                "Native option established before content construction; reuse its layout, appearance and interaction feedback.",
                None,
            ),
        ],
    ));
}

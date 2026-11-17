//! Native compositions keep their cache, observer and construction cursors in the VM graph.
use super::*;
use aside_host::PropertySchema;
use aside_vm::executor::{Continuation, FaultRetention};
use aside_vm::{Heap, ObjectKind, PropertyId, ValueTag};
use reel_ui::stage::{NodeKind, NodeProps, NodeSemantics};

mod api;
mod stack;
mod tabs;
pub(super) use api::{contracts, describe, documentation};

#[derive(aside_host::Properties)]
pub(super) enum PageField {
    /// Selected stable key; null displays no page. Accepts a writable binding.
    #[aside(name="value", ty=Any, default=null)]
    Value,
    /// Fixed unique {key, build} descriptors, copied when this node is constructed.
    #[aside(name="items", ty=Array, readonly)]
    Items,
}
#[derive(aside_host::Properties)]
pub(super) enum TabField {
    /// Selected stable key. Bind the same property to a page stack or another view.
    #[aside(name="value", ty=Any, default=null)]
    Value,
    /// Fixed unique {key, label} descriptors; labels may be bound properties.
    #[aside(name="items", ty=Array, readonly)]
    Items,
    /// Optional no-wait content factory: fn(TabItem) -> (Node | null).
    /// Unannotated lambda parameters inherit the read-only observable TabItem type.
    #[aside(name="build", ty=Any, doc_type=api::tab_factory_type(), default=null, readonly)]
    Build,
    /// Navigation and flow axis: horizontal or vertical.
    #[aside(name="orientation", ty=String, default="horizontal", readonly)]
    Orientation,
    /// Automatic selects on arrows; manual moves focus and selects on confirmation.
    #[aside(name="activation", ty=String, default="automatic", readonly)]
    Activation,
    /// Receives a requested key in event.value. The callback decides when to
    /// assign the selected value and can await page preparation first.
    #[aside(name="on_select", ty=Any, doc_type=aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default=null, validate=validate_tab_selection)]
    OnSelect,
}

fn validate_tab_selection(heap: &Heap, value: Value) -> Result<(), HeapError> {
    stage::validate_button_property(heap, 1, value)
}
const PAGE: i64 = -1501;
const TAB: i64 = -1502;
#[derive(Clone, Copy)]
#[repr(u32)]
enum S {
    Tag,
    Root,
    Entries,
    Sorted,
    Selected,
    Initialized,
    Items,
    SourceVersion,
    SelectionRequest,
}
#[derive(Clone, Copy)]
#[repr(u32)]
enum E {
    Key,
    Build,
    Label,
    Row,
    Node,
    Wrapper,
    Task,
}
const COPY: u64 = 0;
const SORT: u64 = 1;
const WATCH: u64 = 2;
const TAB_BUILD: u64 = 3;
const TAB_BUILT: u64 = 4;
const TAB_ABORT: u64 = 5;
const TAB_ABORT_ERROR: u64 = 100;
const SELECT_METHOD: usize = 5;
const COMMIT_SELECTION: usize = 6;
const AWAIT_TASK: usize = 7;

fn get(h: &Heap, a: Value, i: impl Into<u32>) -> Result<Value, HeapError> {
    h.sequence_get(a, i.into())
}
impl From<S> for u32 {
    fn from(v: S) -> u32 {
        v as u32
    }
}
impl From<E> for u32 {
    fn from(v: E) -> u32 {
        v as u32
    }
}
fn put(h: &mut Heap, a: Value, i: impl Into<u32>, v: Value) -> Result<(), HeapError> {
    h.set_field(a, i.into(), v)
}
fn state(h: &Heap, root: Value) -> Result<Value, HeapError> {
    h.derived_state(h.host_payload(root, 3)?)
}
fn field(h: &Heap, root: Value, i: u32) -> Result<Value, HeapError> {
    h.host_property(PropertyId {
        host: root,
        field: i,
    })
}
fn find(h: &Heap, entries: Value, key: Value, limit: usize) -> Result<Option<Value>, AdapterError> {
    let (mut lo, mut hi) = (0, h.sequence_len(entries)?);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let e = h.sequence_get(entries, mid)?;
        match keys::compare(h, get(h, e, E::Key)?, key, limit)? {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(Some(e)),
        }
    }
    Ok(None)
}
fn descriptor(h: &Heap, object: Value, name: &str, limit: usize) -> Result<Value, AdapterError> {
    let table = match h.info(object)?.kind {
        ObjectKind::Table => object,
        ObjectKind::Host => h.host_fields(object)?,
        _ => return Err(AdapterError::Input("page/tab item must be a record")),
    };
    for i in 0..h.table_len(table)? {
        let (key, value) = h.table_entry(table, i)?;
        if text::read_string(h, key, limit)? == name {
            return Ok(value);
        }
    }
    Err(AdapterError::Input(
        "page/tab item is missing a required field",
    ))
}
fn layout(h: &mut Heap) -> Result<Vec<Value>, HeapError> {
    stage::defaults::layout(h)
}
pub(super) fn validate(h: &Heap, i: u32, v: Value, tab: bool) -> Result<(), HeapError> {
    let prefix = if tab {
        TabField::PROPERTIES.len()
    } else {
        PageField::PROPERTIES.len()
    } as u32;
    if i < prefix {
        if tab {
            TabField::validate(h, i, v)
        } else {
            PageField::validate(h, i, v)
        }
    } else {
        stage::validate_layout_property(h, i - prefix, v)
    }
}
pub(super) fn tab_kind(h: &Heap, fields: Value, limit: usize) -> Result<NodeKind, AdapterError> {
    let orientation = h
        .table_entry(fields, TabField::Orientation.index() as u32)?
        .1;
    let activation = h
        .table_entry(fields, TabField::Activation.index() as u32)?
        .1;
    let orientation = text::read_string(h, orientation, limit)?;
    let activation = text::read_string(h, activation, limit)?;
    if !matches!(activation.as_str(), "automatic" | "manual") {
        return Err(AdapterError::Input(
            "tab activation must be automatic or manual",
        ));
    }
    match orientation.as_str() {
        "horizontal" => Ok(NodeKind::Row(Default::default())),
        "vertical" => Ok(NodeKind::Column(Default::default())),
        _ => Err(AdapterError::Input(
            "tab orientation must be horizontal or vertical",
        )),
    }
}
pub(super) fn semantics(shape: stage::snapshot::Shape, kind: &NodeKind, props: &mut NodeProps) {
    props.semantics = if shape == stage::snapshot::Shape::TabBar {
        NodeSemantics::TabList
    } else if matches!(kind, NodeKind::Radio { tabs: Some(_), .. }) {
        NodeSemantics::Tab
    } else {
        NodeSemantics::Generic
    };
}
pub(super) fn tab_group(
    h: &Heap,
    value: Value,
) -> Result<
    Option<(
        reel_ui::core::SelectionGroupId,
        reel_ui::core::TabNavigation,
    )>,
    AdapterError,
> {
    let parent = h.host_payload(value, 2)?;
    if parent.tag() == ValueTag::Null {
        return Ok(None);
    }
    let output = h.host_payload(parent, 3)?;
    if output.tag() == ValueTag::Null {
        return Ok(None);
    }
    let Ok(s) = h.derived_state(output) else {
        return Ok(None);
    };
    if s.tag() == ValueTag::Null || get(h, s, S::Tag).ok().and_then(Value::as_int) != Some(TAB) {
        return Ok(None);
    }
    let vertical = text::read_string(
        h,
        field(h, parent, TabField::Orientation.index() as u32)?,
        16,
    )? == "vertical";
    let manual = text::read_string(
        h,
        field(h, parent, TabField::Activation.index() as u32)?,
        16,
    )? == "manual";
    Ok(Some((
        reel_ui::core::SelectionGroupId(h.handle(parent)?.key, 0),
        reel_ui::core::TabNavigation { vertical, manual },
    )))
}

impl SessionVm {
    fn page_method(&mut self, index: usize) -> Result<FunctionId, AdapterError> {
        if self.stage.page_methods.is_none() {
            let names = [
                "reel::page_stack::get",
                "reel::page_stack::ensure",
                "reel::page_stack::evict",
                "reel::page_stack::produce",
                "reel::tab_bar::observe_label",
                "reel::page_stack::select",
                "reel::page_stack::commit_selection",
                "reel::completion::await",
            ];
            let mut methods = [None; 8];
            let p = self.vm.program().package();
            for (i, f) in p.functions.iter().enumerate() {
                if let aside_vm::bytecode::FunctionKind::Native(n) = f.kind
                    && let Some(index) = names
                        .iter()
                        .position(|name| p.symbols[p.natives[n.index()].symbol.index()] == *name)
                {
                    methods[index] = Some(FunctionId(i as u32));
                }
            }
            let [
                Some(get),
                Some(ensure),
                Some(evict),
                Some(produce),
                Some(label),
                Some(select),
                Some(commit),
                Some(wait),
            ] = methods
            else {
                return Err(AdapterError::Input("native page adapter missing"));
            };
            self.stage.page_methods =
                Some([get, ensure, evict, produce, label, select, commit, wait]);
        }
        Ok(self.stage.page_methods.unwrap()[index])
    }
    fn page_continue(
        &mut self,
        t: HostTicket,
        values: &[Value],
        phase: u64,
    ) -> Result<(), AdapterError> {
        self.vm.respond(
            t,
            HostResponse::Continue(Continuation {
                version: 1,
                stage: phase,
                values,
                faults: FaultRetention::Preserve,
            }),
        )?;
        Ok(())
    }
    pub(super) fn dispatch_pages(
        &mut self,
        session: &mut Session,
        t: HostTicket,
        symbol: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if budget.with_minimum(1) == 0 {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1,
            });
        }
        if symbol == "reel::tab_bar::observe_label" {
            self.tab_label(t)?;
        } else if symbol.starts_with("reel::page_stack::") {
            self.page_operation(session, t, symbol)?;
        } else {
            return self.page_composition(session, t, symbol == "reel::tab_bar", budget);
        }
        self.issued = None;
        Ok(NativeDispatch::Ready { work: 1 })
    }
    fn page_composition(
        &mut self,
        session: &mut Session,
        t: HostTicket,
        tab: bool,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if t.input == NativeInput::Entry {
            let output = if let Some(output) = self.vm.reactive_output(t)? {
                output
            } else {
                let mut args = self.vm.host_arguments(t)?.to_vec();
                args.push(Value::NULL);
                let origin = self.stage_origin(t);
                let symbol = if tab {
                    "reel::tab_bar"
                } else {
                    "reel::page_stack"
                };
                let root = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    symbol,
                    &args,
                    self.read_limit,
                    origin,
                )?;
                let output = self.vm.heap_mut().alloc_derived(Value::NULL)?;
                let entries = self.vm.heap_mut().alloc_array(&[])?;
                let items = observable::array(self.vm.heap_mut())?;
                let version = self.vm.heap().info(args[1])?.content_version;
                let s = self.vm.heap_mut().alloc_array(&[
                    Value::int(if tab { TAB } else { PAGE }),
                    root,
                    entries,
                    Value::NULL,
                    Value::NULL,
                    Value::bool(false),
                    items,
                    Value::int(version as i64),
                    Value::int(0),
                ])?;
                self.vm.heap_mut().set_derived_state(output, s)?;
                self.vm.heap_mut().set_host_payload(root, 3, output)?;
                if !tab {
                    self.attach_page_methods(root)?;
                }
                output
            };
            let s = self.vm.heap().derived_state(output)?;
            let root = get(self.vm.heap(), s, S::Root)?;
            if self.stage.id(self.vm.heap(), root).is_err() {
                self.vm.heap_mut().disconnect_derived(output)?;
                self.vm.respond(t, HostResponse::Ready(Value::NULL))?;
            } else if get(self.vm.heap(), s, S::Initialized)?.as_bool() == Some(true) {
                self.vm.begin_reactive(
                    t,
                    output,
                    false,
                    Continuation {
                        version: 1,
                        stage: WATCH,
                        values: &[output, s, Value::int(0), Value::NULL],
                        faults: FaultRetention::Preserve,
                    },
                )?;
            } else {
                self.page_continue(t, &[output, s, Value::int(0), Value::NULL], COPY)?;
            }
        } else {
            let values = self.vm.host_continuation(t)?.to_vec();
            let (output, s, index, sort) = (
                values[0],
                values[1],
                values[2].as_int().unwrap() as u32,
                values[3],
            );
            if t.input == NativeInput::CallbackFault {
                self.vm.heap_mut().end_derived(output)?;
                if tab {
                    let fault = self.vm.host_callback_fault(t)?.unwrap();
                    self.vm.heap_mut().disconnect_derived(output)?;
                    self.page_continue(
                        t,
                        &[output, s, Value::int(0), fault.as_value()],
                        TAB_ABORT,
                    )?;
                } else {
                    self.vm.respond(
                        t,
                        HostResponse::ForwardFault(self.vm.host_callback_fault(t)?.unwrap().id),
                    )?;
                }
            } else {
                match t.continuation.unwrap().stage {
                    COPY => {
                        let source = self.vm.host_arguments(t)?[PageField::Items.index()];
                        if self.vm.heap().info(source)?.content_version
                            != get(self.vm.heap(), s, S::SourceVersion)?.as_int().unwrap() as u64
                        {
                            return Err(HeapError::ConcurrentModification.into());
                        }
                        if index < self.vm.heap().sequence_len(source)? {
                            self.copy_page_item(s, source, index, tab)?;
                            self.page_continue(
                                t,
                                &[output, s, Value::int(i64::from(index + 1)), sort],
                                COPY,
                            )?;
                        } else {
                            self.page_continue(t, &[output, s, Value::int(0), sort], SORT)?;
                        }
                    }
                    SORT => {
                        let entries = get(self.vm.heap(), s, S::Entries)?;
                        match lists::sort::advance(
                            self.vm.heap_mut(),
                            entries,
                            sort,
                            budget,
                            self.read_limit,
                        )? {
                            lists::sort::Step::More { state, work } => {
                                self.page_continue(t, &[output, s, Value::int(0), state], SORT)?;
                                self.issued = None;
                                return Ok(NativeDispatch::Ready { work });
                            }
                            lists::sort::Step::Budget { required_work } => {
                                return Ok(NativeDispatch::BudgetYield {
                                    work: 0,
                                    required_work,
                                });
                            }
                            lists::sort::Step::Ready { result, work } => {
                                put(self.vm.heap_mut(), s, S::Sorted, result)?;
                                let root = get(self.vm.heap(), s, S::Root)?;
                                let items = get(self.vm.heap(), s, S::Items)?;
                                let fields = self.vm.heap().host_fields(root)?;
                                let index = if tab {
                                    TabField::Items.index()
                                } else {
                                    PageField::Items.index()
                                } as u32;
                                let name = self.vm.heap().table_entry(fields, index)?.0;
                                self.vm.heap_mut().table_store(fields, index, name, items)?;
                                self.vm.begin_reactive(
                                    t,
                                    output,
                                    false,
                                    Continuation {
                                        version: 1,
                                        stage: WATCH,
                                        values: &[output, s, Value::int(0), Value::NULL],
                                        faults: FaultRetention::Preserve,
                                    },
                                )?;
                                self.issued = None;
                                return Ok(NativeDispatch::Ready { work });
                            }
                        }
                    }
                    WATCH => {
                        if tab {
                            self.tabs_watch(t, output, s)?;
                        } else {
                            self.pages_watch(session, t, output, s)?;
                        }
                    }
                    TAB_BUILD | TAB_BUILT => {
                        if let Err(error) = self.tabs_build(
                            session,
                            t,
                            output,
                            s,
                            index,
                            t.continuation.unwrap().stage == TAB_BUILT,
                        ) {
                            self.tabs_abort_error(t, output, s, error)?;
                        }
                    }
                    TAB_ABORT => self.tabs_abort(t, output, s, index, sort, None)?,
                    phase if phase >= TAB_ABORT_ERROR => {
                        let kind = [
                            FaultKind::Type,
                            FaultKind::Resource,
                            FaultKind::ReadOnly,
                            FaultKind::InvalidState,
                            FaultKind::ConcurrentModification,
                            FaultKind::OutOfBounds,
                            FaultKind::Effect,
                        ]
                        .into_iter()
                        .find(|kind| TAB_ABORT_ERROR + *kind as u64 == phase)
                        .ok_or(AdapterError::Input("tab cleanup fault kind"))?;
                        self.tabs_abort(t, output, s, index, sort, Some(kind))?;
                    }
                    _ => return Err(AdapterError::Input("page/tab continuation")),
                }
            }
        }
        self.issued = None;
        Ok(NativeDispatch::Ready { work: 1 })
    }
    fn copy_page_item(
        &mut self,
        s: Value,
        source: Value,
        index: u32,
        tab: bool,
    ) -> Result<(), AdapterError> {
        let item = self.vm.heap().sequence_get(source, index)?;
        let key = descriptor(self.vm.heap(), item, "key", self.read_limit)?;
        if key.tag() == ValueTag::Null {
            return Err(AdapterError::Input("page/tab key must not be null"));
        }
        let (build, label) = if tab {
            (
                Value::NULL,
                descriptor(self.vm.heap(), item, "label", self.read_limit)?,
            )
        } else {
            (
                descriptor(self.vm.heap(), item, "build", self.read_limit)?,
                Value::NULL,
            )
        };
        if !tab
            && !self
                .vm
                .heap()
                .accepts_builtin(aside_vm::bytecode::BuiltinType::Closure, build)?
        {
            return Err(AdapterError::Input("page build must be a function"));
        }
        let copied_fields = if tab {
            [("key", key), ("label", label)]
        } else {
            [("key", key), ("build", build)]
        };
        let copied = observable::record(self.vm.heap_mut(), &copied_fields, &[])?;
        let items = get(self.vm.heap(), s, S::Items)?;
        self.vm.heap_mut().array_push(items, copied)?;
        let e = self.vm.heap_mut().alloc_array(&[
            key,
            build,
            label,
            Value::NULL,
            Value::NULL,
            Value::NULL,
            Value::NULL,
        ])?;
        self.vm.heap_mut().observe_readonly_collection(e)?;
        let entries = get(self.vm.heap(), s, S::Entries)?;
        self.vm.heap_mut().array_push(entries, e)?;
        Ok(())
    }
}

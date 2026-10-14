//! Stable Aside host nodes projected into Reel's one shared stage model.
use std::collections::BTreeMap;
pub(super) mod controls;
pub(super) mod input;
mod interaction;

use aside_vm::{
    Heap, HeapError, HeapHandle, ObjectKind, PropertyId, RootKind, RootOwner, RootToken, Value,
    ValueTag,
};
use reel_ui::stage::{NodeId, NodeKind, NodeProps, Stage};

use super::{AdapterError, registry, text};
use aside_host::PropertySchema;
use aside_host::{CallContext, NativeBinding};
use aside_vm::bytecode::{Effects, FunctionId, FunctionKind, VerifiedProgram};
use aside_vm::executor::Vm;

/// One host schema feeds construction, writes and animation; NodeProps remains in reel-ui.
#[derive(aside_host::Properties)]
pub(super) enum NodeProperty {
    #[aside(name = "x", ty = Any, default = 0, validate = validate_number, animatable)]
    X,
    #[aside(name = "y", ty = Any, default = 0, validate = validate_number, animatable)]
    Y,
    #[aside(name = "width", ty = Any, default = null, validate = validate_dimension, animatable)]
    Width,
    #[aside(name = "height", ty = Any, default = null, validate = validate_dimension, animatable)]
    Height,
    #[aside(name = "opacity", ty = Any, default = 1, validate = validate_opacity, animatable)]
    Opacity,
    #[aside(name = "visible", ty = Bool, default = true)]
    Visible,
    #[aside(name = "enabled", ty = Bool, default = true)]
    Enabled,
    #[aside(name = "modal", ty = Bool, default = false)]
    Modal,
    #[aside(name = "clip", ty = Bool, default = false)]
    Clip,
    #[aside(name = "gap", ty = Any, default = 0, validate = validate_nonnegative, animatable)]
    Gap,
    #[aside(name = "padding", ty = Any, default = 0, validate = validate_nonnegative, animatable)]
    Padding,
    #[aside(name = "font_size", ty = Any, default = 24, validate = validate_nonnegative, animatable)]
    FontSize,
    #[aside(name = "font", ty = String, default = "system-ui")]
    Font,
    #[aside(name = "color", ty = String, default = "#ffffff", validate = validate_color)]
    Color,
    #[aside(name = "background", ty = Any, default = null, validate = validate_background)]
    Background,
    #[aside(name = "rotation", ty = Any, default = 0, validate = validate_number, animatable)]
    Rotation,
    #[aside(name = "scale_x", ty = Any, default = 1, validate = validate_number, animatable)]
    ScaleX,
    #[aside(name = "scale_y", ty = Any, default = 1, validate = validate_number, animatable)]
    ScaleY,
    #[aside(name = "anchor_x", ty = Any, default = 0, validate = validate_number, animatable)]
    AnchorX,
    #[aside(name = "anchor_y", ty = Any, default = 0, validate = validate_number, animatable)]
    AnchorY,
    #[aside(name = "decorated", ty = Bool, default = true)]
    Decorated,
    #[aside(name = "reveal", ty = Any, default = null, validate = validate_reveal)]
    Reveal,
}

pub(super) struct StageBinding {
    pub scene: Stage,
    handles: BTreeMap<NodeId, NodeBinding>,
    root: Option<RootToken>,
    methods: [FunctionId; 4],
}
struct NodeBinding {
    handle: HeapHandle,
    interaction_offset: u32,
    fields_version: u64,
    input_source: Option<HeapHandle>,
    text_source: Option<HeapHandle>,
    input_values: BTreeMap<std::sync::Arc<str>, f32>,
}

impl StageBinding {
    pub fn new(program: &VerifiedProgram) -> Result<Self, AdapterError> {
        let p = program.package();
        let mut methods = [FunctionId(0); 4];
        for (slot, symbol) in methods.iter_mut().zip([
            add::SYMBOL,
            remove::SYMBOL,
            clear::SYMBOL,
            input::value::SYMBOL,
        ]) {
            *slot = p
                .functions
                .iter()
                .enumerate()
                .find_map(|(i, f)| match f.kind {
                    FunctionKind::Native(native)
                        if p.symbols[p.natives[native.index()].symbol.index()] == symbol =>
                    {
                        Some(FunctionId(i as u32))
                    }
                    _ => None,
                })
                .ok_or(AdapterError::Input("node method callable missing"))?;
        }
        Ok(Self {
            scene: Stage::default(),
            handles: BTreeMap::new(),
            root: None,
            methods,
        })
    }
    pub(super) fn replace_view(
        &mut self,
        heap: &mut Heap,
        container: Value,
        child: Value,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let id = self.id(heap, container)?;
        let old = self.scene.node(id).unwrap().children.clone();
        let references = heap.alloc_array(&[])?;
        if child.tag() != ValueTag::Null {
            let child_id = self.id(heap, child)?;
            if old.as_slice() == [child_id] {
                return Ok(());
            }
            self.sync_tree(heap, child_id, read_limit)?;
            heap.array_push(references, child)?;
            // Admission precedes retirement: a failed mount keeps the old tree.
            self.scene.add(id, child_id, old.len())?;
            heap.set_host_payload(child, 2, container)?;
        }
        for child in old {
            let retired = self.scene.remove(child)?;
            self.retire(heap, retired)?;
        }
        heap.set_host_payload(container, 1, references)?;
        Ok(())
    }

    pub(super) fn id(&self, heap: &Heap, value: Value) -> Result<NodeId, AdapterError> {
        heap.host_fields(value)?;
        let id = NodeId(
            heap.host_payload(value, 0)?
                .as_int()
                .ok_or(AdapterError::Input("expected stage node"))? as u64,
        );
        if self.handles.get(&id).map(|node| node.handle) != Some(heap.handle(value)?)
            || self.scene.node(id).is_none()
        {
            return Err(AdapterError::Input("node is not owned by this stage"));
        }
        Ok(id)
    }

    pub(super) fn property_offset(&self, heap: &Heap, value: Value) -> Result<u32, AdapterError> {
        let id = self.id(heap, value)?;
        Ok(match self.scene.node(id).unwrap().kind {
            NodeKind::Text(_) => 1,
            NodeKind::Slider { .. } => controls::SliderField::PROPERTIES.len() as u32,
            NodeKind::Toggle { .. } => controls::ToggleField::PROPERTIES.len() as u32,
            NodeKind::Image { .. } | NodeKind::Button { .. } | NodeKind::Input { .. } => 2,
            _ => 0,
        })
    }

    pub fn dispatch(
        &mut self,
        vm: &mut Vm,
        owner: RootOwner,
        symbol: &str,
        args: &[Value],
        read_limit: usize,
        origin: Option<String>,
    ) -> Result<Value, AdapterError> {
        let heap = vm.heap_mut();
        if symbol == "reel::stage::replace" {
            let node = args[0];
            let id = self.id(heap, node)?;
            self.sync_tree(heap, id, read_limit)?;
            let root = heap.add_root(owner, RootKind::Host, node)?;
            let retired = match self.scene.replace(id) {
                Ok(retired) => retired,
                Err(error) => {
                    heap.release_root(root)?;
                    return Err(error.into());
                }
            };
            if let Some(old) = self.root.replace(root) {
                heap.release_root(old)?;
            }
            self.retire(heap, retired)?;
            return Ok(Value::NULL);
        }

        let raw_args = args;
        let resolved = args
            .iter()
            .map(|&value| heap.property_argument(value))
            .collect::<Result<Vec<_>, _>>()?;
        let args = resolved.as_slice();
        let prefix = registry::node_prefix(symbol);
        let fields = heap.alloc_table()?;
        let field_count = prefix.len() + NodeProperty::PROPERTIES.len();
        for (index, argument) in args.iter().take(field_count).enumerate() {
            let name = if index < prefix.len() {
                prefix[index]
            } else {
                NodeProperty::PROPERTIES[index - prefix.len()].name
            };
            let key = heap.alloc_string(name)?;
            heap.table_store(fields, index as u32, key, *argument)?;
        }
        let kind = match symbol {
            "reel::text" => NodeKind::Text(read_text(heap, args[0], read_limit)?.into()),
            "reel::slider" => controls::slider_kind(heap, fields)?,
            "reel::toggle" => controls::toggle_kind(heap, fields, read_limit)?,
            "reel::button" => button_kind(heap, args[0], args[1], read_limit)?,
            "reel::input" => {
                input::validate(heap, 1, args[1])?;
                NodeKind::Input {
                    bindings: input::read_bindings(heap, args[0])?,
                    listening: args[1].tag() != ValueTag::Null,
                }
            }
            "reel::image" => NodeKind::Image {
                source: read_source(heap, args[0], read_limit)?,
                fit: read_fit(heap, args[1])?,
                origin,
            },
            "reel::row" => NodeKind::Row,
            "reel::column" => NodeKind::Column,
            "reel::stack" => NodeKind::Stack,
            _ => unreachable!("registered stage constructor"),
        };
        let props = read_props(heap, fields, prefix.len(), read_limit)?;
        let mut children = Vec::new();
        let references = heap.alloc_array(&[])?;
        if matches!(
            kind,
            NodeKind::Row
                | NodeKind::Column
                | NodeKind::Stack
                | NodeKind::Button { .. }
                | NodeKind::Toggle { .. }
                | NodeKind::Input { .. }
        ) {
            let input = args[args.len() - 1];
            if input.tag() != ValueTag::Null {
                for index in 0..heap.sequence_len(input)? {
                    let child = heap.sequence_get(input, index)?;
                    children.push(self.id(heap, child)?);
                    heap.array_push(references, child)?;
                }
            }
        }
        let id = self.scene.next_identity()?;
        let value = heap.alloc_composable_host(
            fields,
            &[
                Value::int(id.0 as i64),
                references,
                Value::NULL,
                Value::NULL,
            ],
            Some(match symbol {
                "reel::slider" => controls::validate_slider,
                "reel::toggle" => {
                    |heap, _, index, value| controls::validate_toggle(heap, index, value)
                }
                "reel::text" => |heap, _, index, value| validate_text_property(heap, index, value),
                "reel::image" => {
                    |heap, _, index, value| validate_image_property(heap, index, value)
                }
                "reel::button" => {
                    |heap, _, index, value| validate_button_property(heap, index, value)
                }
                "reel::input" => |heap, _, index, value| input::validate(heap, index, value),
                _ => |heap, _, index, value| validate_layout_property(heap, index, value),
            }),
        )?;
        for (offset, name) in interaction::FIELDS.into_iter().enumerate() {
            let key = heap.alloc_string(name)?;
            let state = name == "disabled" && !props.enabled;
            heap.table_store(
                fields,
                (field_count + offset) as u32,
                key,
                Value::bool(state),
            )?;
        }
        heap.observe_host_properties(value, field_count as u32, interaction::FIELDS.len() as u32)?;
        let control_field = match symbol {
            "reel::slider" => Some(controls::SliderField::Value.index()),
            "reel::toggle" => Some(controls::ToggleField::Value.index()),
            _ => None,
        };
        if let Some(field) = control_field {
            heap.require_writable_property(PropertyId {
                host: value,
                field: field as u32,
            })?;
        }

        for (field, &argument) in raw_args.iter().take(field_count).enumerate() {
            if heap.is_property_ref(argument) {
                heap.bind_host_property(
                    PropertyId {
                        host: value,
                        field: field as u32,
                    },
                    argument,
                )?;
            }
        }
        for (i, (name, function)) in ["add", "remove", "clear", "value"]
            .into_iter()
            .zip(self.methods)
            .enumerate()
        {
            if name == "value" && symbol != "reel::input" {
                continue;
            }
            let method = vm.bind_native(value, function)?;
            let heap = vm.heap_mut();
            let key = heap.alloc_string(name)?;
            heap.table_store(
                fields,
                (field_count + interaction::FIELDS.len() + i) as u32,
                key,
                method,
            )?;
        }
        let heap = vm.heap_mut();
        let input_values = match &kind {
            NodeKind::Input { bindings, .. } => {
                bindings.iter().map(|a| (a.name.clone(), 0.)).collect()
            }
            _ => BTreeMap::new(),
        };
        self.scene.create(kind, props, children)?;
        // Keeping any part of a component keeps its ancestors and siblings
        // alive. Explicit retirement clears both directions of these edges.
        for index in 0..heap.sequence_len(references)? {
            let child = heap.sequence_get(references, index)?;
            heap.set_host_payload(child, 2, value)?;
        }
        self.handles.insert(
            id,
            NodeBinding {
                handle: heap.handle(value)?,
                interaction_offset: field_count as u32,
                fields_version: heap.info(fields)?.content_version,
                input_source: if symbol == "reel::input" {
                    Some(heap.handle(args[0])?)
                } else {
                    None
                },
                text_source: if symbol == "reel::text" {
                    Some(heap.handle(args[0])?)
                } else {
                    None
                },
                input_values,
            },
        );
        Ok(value)
    }

    fn sync_tree(
        &mut self,
        heap: &Heap,
        root: NodeId,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let mut pending = vec![root];
        let mut updates = Vec::new();
        while let Some(id) = pending.pop() {
            let node = self
                .scene
                .node(id)
                .ok_or(AdapterError::Input("retired stage node"))?;
            pending.extend(node.children.iter().copied());
            let binding = &self.handles[&id];
            let value = heap.import_handle(binding.handle)?;
            let fields = heap.host_fields(value)?;
            let version = heap.info(fields)?.content_version;
            if version == binding.fields_version {
                continue;
            }
            let (kind, offset) = match &node.kind {
                NodeKind::Input { bindings, .. } => {
                    let source = heap.table_entry(fields, 0)?.1;
                    let bindings = if binding.input_source == Some(heap.handle(source)?) {
                        bindings.clone()
                    } else {
                        input::read_bindings(heap, source)?
                    };
                    let callback = heap.table_entry(fields, 1)?.1;
                    input::validate(heap, 1, callback)?;
                    (
                        NodeKind::Input {
                            bindings,
                            listening: callback.tag() != ValueTag::Null,
                        },
                        2,
                    )
                }
                NodeKind::Slider { .. } => (
                    controls::slider_kind(heap, fields)?,
                    controls::SliderField::PROPERTIES.len(),
                ),
                NodeKind::Toggle { .. } => (
                    controls::toggle_kind(heap, fields, read_limit)?,
                    controls::ToggleField::PROPERTIES.len(),
                ),
                NodeKind::Button { .. } => (
                    button_kind(
                        heap,
                        heap.table_entry(fields, 0)?.1,
                        heap.table_entry(fields, 1)?.1,
                        read_limit,
                    )?,
                    2,
                ),
                NodeKind::Text(text) => {
                    let source = heap.table_entry(fields, 0)?.1;
                    (
                        NodeKind::Text(if binding.text_source == Some(heap.handle(source)?) {
                            text.clone()
                        } else {
                            read_text(heap, source, read_limit)?.into()
                        }),
                        1,
                    )
                }
                NodeKind::Image { origin, .. } => (
                    NodeKind::Image {
                        source: read_source(heap, heap.table_entry(fields, 0)?.1, read_limit)?,
                        fit: read_fit(heap, heap.table_entry(fields, 1)?.1)?,
                        origin: origin.clone(),
                    },
                    2,
                ),
                kind => (kind.clone(), 0),
            };
            let props = read_props(heap, fields, offset, read_limit)?;
            updates.push((id, kind, props, version));
        }
        for (id, kind, props, version) in updates {
            if matches!(kind, NodeKind::Text(_)) {
                let value = heap.import_handle(self.handles[&id].handle)?;
                let source = heap.table_entry(heap.host_fields(value)?, 0)?.1;
                self.handles.get_mut(&id).unwrap().text_source = Some(heap.handle(source)?);
            }
            if let NodeKind::Input { bindings, .. } = &kind {
                let binding = self.handles.get_mut(&id).unwrap();
                let source = heap
                    .table_entry(heap.host_fields(heap.import_handle(binding.handle)?)?, 0)?
                    .1;
                if binding.input_source != Some(heap.handle(source)?) {
                    binding.input_values = bindings.iter().map(|a| (a.name.clone(), 0.)).collect();
                    binding.input_source = Some(heap.handle(source)?);
                }
            }
            self.scene.update(id, kind, props)?;
            self.handles.get_mut(&id).unwrap().fields_version = version;
        }
        Ok(())
    }

    pub fn sync(&mut self, heap: &mut Heap, read_limit: usize) -> Result<(), AdapterError> {
        if let Some(root) = self.scene.root() {
            self.sync_tree(heap, root, read_limit)?;
        }
        // Every node keeps its complete component alive. A collected detached
        // root therefore means the whole component has lost script ownership.
        let lost = self
            .scene
            .nodes()
            .filter(|node| node.parent.is_none() && Some(node.id) != self.scene.root())
            .filter(|node| heap.import_handle(self.handles[&node.id].handle).is_err())
            .map(|node| node.id)
            .collect::<Vec<_>>();
        for root in lost {
            let retired = self.scene.discard(root)?;
            self.retire(heap, retired)?;
        }
        Ok(())
    }

    fn retire(&mut self, heap: &mut Heap, nodes: Vec<NodeId>) -> Result<(), AdapterError> {
        for id in nodes {
            if let Some(binding) = self.handles.remove(&id)
                && let Ok(value) = heap.import_handle(binding.handle)
            {
                interaction::publish(heap, value, binding.interaction_offset, Default::default())?;
                let view = heap.host_payload(value, 3)?;
                if view.tag() != ValueTag::Null {
                    heap.disconnect_derived(view)?;
                }
                heap.retire_host(value)?;
            }
        }
        Ok(())
    }

    pub fn revoke(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        if let Some(root) = self.root.take() {
            heap.release_root(root)?;
        }
        let nodes = self.handles.keys().copied().collect();
        self.retire(heap, nodes)?;
        self.scene = Stage::default();
        Ok(())
    }

    pub fn value(&self, heap: &Heap, id: NodeId) -> Result<Value, AdapterError> {
        let node = self
            .handles
            .get(&id)
            .ok_or(AdapterError::Input("retired stage node"))?;
        Ok(heap.import_handle(node.handle)?)
    }

    /// Input has already been routed through accepted geometry. Check current
    /// ownership and script fields before retaining a callback task.
    pub fn click(&self, heap: &Heap, id: NodeId) -> Result<Option<(Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(id) else {
            return Ok(None);
        };
        if !matches!(node.kind, NodeKind::Button { .. }) {
            return Ok(None);
        }
        let target = self.value(heap, id)?;
        let callback = heap.table_entry(heap.host_fields(target)?, 1)?.1;
        if callback.tag() == ValueTag::Null {
            return Ok(None);
        }
        Ok(self
            .mounted_in_scope(heap, id, self.modal_scope(heap)?)?
            .then_some((target, callback)))
    }

    fn mounted_in_scope(
        &self,
        heap: &Heap,
        id: NodeId,
        modal: Option<NodeId>,
    ) -> Result<bool, AdapterError> {
        let mut in_scope = modal.is_none();
        let mut current = Some(id);
        let mut root = id;
        while let Some(id) = current {
            in_scope |= modal == Some(id);
            let value = self.value(heap, id)?;
            let fields = heap.host_fields(value)?;
            let offset = self.property_offset(heap, value)?;
            for property in [NodeProperty::Visible, NodeProperty::Enabled] {
                if heap
                    .table_entry(fields, offset + property.index() as u32)?
                    .1
                    .as_bool()
                    != Some(true)
                {
                    return Ok(false);
                }
            }
            root = id;
            current = self.scene.node(id).unwrap().parent;
        }
        Ok(in_scope && self.scene.root() == Some(root))
    }

    /// Inspect current script flags, including writes not yet projected to the UI.
    /// Reverse paint order stops at the first visible modal; no strings or full
    /// node properties are copied on the activation path.
    fn modal_scope(&self, heap: &Heap) -> Result<Option<NodeId>, AdapterError> {
        let mut pending: Vec<_> = self
            .scene
            .root()
            .map(|id| (id, false))
            .into_iter()
            .collect();
        while let Some((id, children_visited)) = pending.pop() {
            let value = self.value(heap, id)?;
            let fields = heap.host_fields(value)?;
            let offset = self.property_offset(heap, value)?;
            let flag = |property: NodeProperty| -> Result<bool, AdapterError> {
                Ok(heap
                    .table_entry(fields, offset + property.index() as u32)?
                    .1
                    .as_bool()
                    == Some(true))
            };
            if !flag(NodeProperty::Visible)? {
                continue;
            }
            if children_visited {
                if flag(NodeProperty::Modal)? {
                    return Ok(Some(id));
                }
            } else {
                pending.push((id, true));
                pending.extend(
                    self.scene
                        .node(id)
                        .unwrap()
                        .children
                        .iter()
                        .map(|&id| (id, false)),
                );
            }
        }
        Ok(None)
    }

    pub fn has_handlers(&self) -> bool {
        let mut pending: Vec<_> = self.scene.root().into_iter().collect();
        while let Some(id) = pending.pop() {
            let node = self.scene.node(id).unwrap();
            if match &node.kind {
                NodeKind::Slider { .. } | NodeKind::Toggle { .. } => true,
                NodeKind::Button { listening, .. } => *listening,
                NodeKind::Input { bindings, .. } => !bindings.is_empty(),
                _ => false,
            } {
                return true;
            }
            pending.extend(node.children.iter().copied());
        }
        false
    }
}

pub(super) fn bindings() -> [NativeBinding<StageBinding, AdapterError>; 5] {
    [
        add::binding(),
        remove::binding(),
        clear::binding(),
        input::binding::binding(),
        input::value::binding(),
    ]
}

#[aside_host::native(symbol = "reel::node::add", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn add(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
    child: Value,
    #[aside(default = null)] index: Option<i64>,
) -> Result<Value, AdapterError> {
    let heap = context.vm.heap_mut();
    let parent_id = context.host.id(heap, receiver)?;
    let child_id = context.host.id(heap, child)?;
    let node = context.host.scene.node(parent_id).unwrap();
    let index = match index {
        Some(i) => usize::try_from(i)
            .map_err(|_| AdapterError::Input("child index must be nonnegative"))?,
        None => node.children.len(),
    };
    // Prepare replacement traced edges before committing either tree.
    let previous = heap.host_payload(receiver, 1)?;
    let next = heap.alloc_array(&[])?;
    for i in 0..heap.sequence_len(previous)? {
        if i as usize == index {
            heap.array_push(next, child)?;
        }
        heap.array_push(next, heap.sequence_get(previous, i)?)?;
    }
    if index == node.children.len() {
        heap.array_push(next, child)?;
    }
    context.host.scene.add(parent_id, child_id, index)?;
    heap.set_host_payload(receiver, 1, next)?;
    heap.set_host_payload(child, 2, receiver)?;
    Ok(child)
}

#[aside_host::native(symbol = "reel::node::remove", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn remove(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
) -> Result<(), AdapterError> {
    let heap = context.vm.heap_mut();
    if matches!(heap.host_fields(receiver), Err(HeapError::RetiredHost)) {
        return Ok(());
    }
    let id = context.host.id(heap, receiver)?;
    let parent = heap.host_payload(receiver, 2)?;
    let edges = if parent.tag() != ValueTag::Null {
        let previous = heap.host_payload(parent, 1)?;
        let next = heap.alloc_array(&[])?;
        for i in 0..heap.sequence_len(previous)? {
            let child = heap.sequence_get(previous, i)?;
            if !child.same_bits(receiver) {
                heap.array_push(next, child)?;
            }
        }
        Some(next)
    } else {
        None
    };
    let was_root = context.host.scene.root() == Some(id);
    let retired = context.host.scene.remove(id)?;
    if let Some(edges) = edges {
        heap.set_host_payload(parent, 1, edges)?;
    }
    if was_root && let Some(root) = context.host.root.take() {
        heap.release_root(root)?;
    }
    context.host.retire(heap, retired)
}

#[aside_host::native(symbol = "reel::node::clear", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn clear(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
) -> Result<(), AdapterError> {
    let heap = context.vm.heap_mut();
    let id = context.host.id(heap, receiver)?;
    let edges = heap.alloc_array(&[])?;
    let retired = context.host.scene.clear_children(id)?;
    heap.set_host_payload(receiver, 1, edges)?;
    context.host.retire(heap, retired)
}

fn read_text(heap: &Heap, value: Value, read_limit: usize) -> Result<String, AdapterError> {
    text::read_string(heap, value, read_limit)
}

fn read_source(heap: &Heap, value: Value, read_limit: usize) -> Result<String, AdapterError> {
    let source = read_text(heap, value, read_limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("image source must not be empty"));
    }
    Ok(source)
}

fn read_fit(heap: &Heap, value: Value) -> Result<reel_ui::properties::ObjectFit, AdapterError> {
    use reel_ui::properties::ObjectFit;
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        ));
    }
    let mut bytes = [0; 7];
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    match &bytes[..info.len as usize] {
        b"contain" => Ok(ObjectFit::Contain),
        b"cover" => Ok(ObjectFit::Cover),
        b"stretch" => Ok(ObjectFit::Stretch),
        _ => Err(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        )),
    }
}

fn validate_image_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    match index {
        0 => {
            let info = heap.info(value)?;
            if info.kind == ObjectKind::String && info.len != 0 {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            }
        }
        1 => read_fit(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        _ => validate_layout_property(heap, index - 2, value),
    }
}

fn button_kind(
    heap: &Heap,
    label: Value,
    callback: Value,
    read_limit: usize,
) -> Result<NodeKind, AdapterError> {
    validate_button_property(heap, 1, callback)?;
    Ok(NodeKind::Button {
        label: if label.tag() == ValueTag::Null {
            None
        } else {
            Some(read_text(heap, label, read_limit)?)
        },
        listening: callback.tag() != ValueTag::Null,
    })
}
fn validate_button_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    match index {
        0 | 1 if value.tag() == ValueTag::Null => Ok(()),
        0 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::String, value)? => Ok(()),
        1 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::Closure, value)? => Ok(()),
        0 | 1 => Err(HeapError::TypeMismatch),
        _ => validate_layout_property(heap, index - 2, value),
    }
}

pub(super) fn number(value: Value) -> Result<f64, AdapterError> {
    let value = value
        .as_float()
        .or_else(|| value.as_int().map(|n| n as f64))
        .ok_or(AdapterError::Input("node property requires a number"))?;
    if !value.is_finite() {
        return Err(AdapterError::Input("node geometry must be finite"));
    }
    Ok(value)
}
fn nonnegative(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if value < 0. {
        return Err(AdapterError::Input("node size must be nonnegative"));
    }
    Ok(value)
}
fn dimension(value: Value) -> Result<Option<f64>, AdapterError> {
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        nonnegative(value).map(Some)
    }
}
fn opacity(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input("opacity must be between zero and one"));
    }
    Ok(value)
}
fn boolean(value: Value) -> Result<bool, AdapterError> {
    value
        .as_bool()
        .ok_or(AdapterError::Input("node property requires bool"))
}
fn color(heap: &Heap, value: Value) -> Result<[u8; 4], AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || !matches!(info.len, 7 | 9) {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut bytes = [0; 9];
    // A color has at most nine bytes. Single-byte reads also respect the Heap's
    // existing per-operation byte budget, without allocating or copying text.
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    if bytes[0] != b'#' {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut result = [255; 4];
    for (channel, pair) in result
        .iter_mut()
        .zip(bytes[1..info.len as usize].chunks_exact(2))
    {
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(AdapterError::Input("color requires hexadecimal channels")),
        };
        *channel = hex(pair[0])? * 16 + hex(pair[1])?;
    }
    Ok(result)
}
fn validate_text_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    if index == 0 {
        return if heap.info(value)?.kind == ObjectKind::String {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        };
    }
    validate_layout_property(heap, index - 1, value)
}
pub(super) fn validate_layout_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    if index as usize >= NodeProperty::PROPERTIES.len() {
        return if (index as usize) < NodeProperty::PROPERTIES.len() + interaction::FIELDS.len()
            && value.as_bool().is_some()
        {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        };
    }
    NodeProperty::validate(heap, index, value)
}
fn validation<T>(result: Result<T, AdapterError>) -> Result<(), HeapError> {
    result.map(|_| ()).map_err(|error| match error {
        AdapterError::Heap(error) => error,
        _ => HeapError::TypeMismatch,
    })
}
fn validate_number(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(number(value))
}
fn validate_dimension(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(dimension(value))
}
fn validate_opacity(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(opacity(value))
}
fn validate_nonnegative(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(nonnegative(value))
}
fn validate_color(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validation(color(heap, value))
}
fn validate_background(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_color(heap, value)
    }
}
fn validate_reveal(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null || value.as_int().is_some_and(|n| usize::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
fn read_props(
    heap: &Heap,
    fields: Value,
    offset: usize,
    read_limit: usize,
) -> Result<NodeProps, AdapterError> {
    let get = |property: NodeProperty| -> Result<Value, AdapterError> {
        Ok(heap
            .table_entry(fields, (property.index() + offset) as u32)?
            .1)
    };
    let background = get(NodeProperty::Background)?;
    Ok(NodeProps {
        x: number(get(NodeProperty::X)?)?,
        y: number(get(NodeProperty::Y)?)?,
        width: dimension(get(NodeProperty::Width)?)?,
        height: dimension(get(NodeProperty::Height)?)?,
        opacity: opacity(get(NodeProperty::Opacity)?)?,
        visible: boolean(get(NodeProperty::Visible)?)?,
        enabled: boolean(get(NodeProperty::Enabled)?)?,
        modal: boolean(get(NodeProperty::Modal)?)?,
        clip: boolean(get(NodeProperty::Clip)?)?,
        decorated: boolean(get(NodeProperty::Decorated)?)?,
        reveal: get(NodeProperty::Reveal)?.as_int().map(|v| v as usize),
        gap: nonnegative(get(NodeProperty::Gap)?)?,
        padding: nonnegative(get(NodeProperty::Padding)?)?,
        font_size: nonnegative(get(NodeProperty::FontSize)?)?,
        font: text::read_string(heap, get(NodeProperty::Font)?, read_limit)?,
        color: color(heap, get(NodeProperty::Color)?)?,
        background: if background.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, background)?)
        },
        rotation: number(get(NodeProperty::Rotation)?)?,
        scale_x: number(get(NodeProperty::ScaleX)?)?,
        scale_y: number(get(NodeProperty::ScaleY)?)?,
        anchor_x: number(get(NodeProperty::AnchorX)?)?,
        anchor_y: number(get(NodeProperty::AnchorY)?)?,
    })
}

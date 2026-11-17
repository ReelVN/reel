//! Current native identities for the shared logical data graph.
use super::*;
use crate::aside::data::{LogicalReference, LogicalReferences, TypedDataError};
use std::cell::RefCell;
mod connections;
mod curve;
mod random;
mod schema;
pub(super) use connections::Restoring;

/// A candidate stage adapter. The typed graph owns every allocated native value;
/// the stage stores weak handles and never mounts a restored node automatically.
pub(in crate::aside) struct SceneRestore<'a> {
    stage: RefCell<&'a mut StageBinding>,
    activities: RefCell<&'a mut crate::aside::activity::Activities>,
    media: Option<RefCell<&'a mut crate::aside::media::Media>>,
    owner: RootOwner,
    limit: usize,
}
impl<'a> SceneRestore<'a> {
    pub(in crate::aside) fn new(
        stage: &'a mut StageBinding,
        activities: &'a mut crate::aside::activity::Activities,
        owner: RootOwner,
        limit: usize,
    ) -> Self {
        Self {
            stage: RefCell::new(stage),
            activities: RefCell::new(activities),
            media: None,
            owner,
            limit,
        }
    }
    pub(in crate::aside) fn with_media(
        mut self,
        media: &'a mut crate::aside::media::Media,
    ) -> Self {
        self.media = Some(RefCell::new(media));
        self
    }
}
/// Read-only capture borrows the stage independently from the VM heap.
pub(in crate::aside) struct SceneCapture<'a> {
    stage: &'a StageBinding,
    activities: &'a crate::aside::activity::Activities,
    default_owner: Value,
    media: Option<&'a crate::aside::media::MediaCapture>,
}
impl<'a> SceneCapture<'a> {
    pub(in crate::aside) fn new(
        stage: &'a StageBinding,
        activities: &'a crate::aside::activity::Activities,
        _: usize,
    ) -> Self {
        Self {
            stage,
            activities,
            default_owner: Value::NULL,
            media: None,
        }
    }
    pub(in crate::aside) fn with_default_owner(mut self, owner: Value) -> Self {
        self.default_owner = owner;
        self
    }
    pub(in crate::aside) fn with_media(
        mut self,
        media: &'a crate::aside::media::MediaCapture,
    ) -> Self {
        self.media = Some(media);
        self
    }
}
/// Keep author nodes as graph edges and reconnect the current Root's scope on adoption.
pub(in crate::aside) fn logical_owner(
    heap: &Heap,
    stage: &StageBinding,
    owner: Value,
    default_owner: Value,
) -> Result<(Value, bool), TypedDataError> {
    if owner.tag() == ValueTag::Null || stage.id(heap, owner).is_ok() {
        return Ok((owner, false));
    }
    if owner.same_bits(default_owner) && super::super::scopes::is_scope(heap, owner) {
        return Ok((Value::NULL, true));
    }
    Err(TypedDataError::Contract(
        "logical resource owner must be a Node, the current Root scope or null",
    ))
}
fn shape(kind: &str) -> Result<snapshot::Shape, TypedDataError> {
    schema::shape(kind).ok_or(TypedDataError::Contract(
        "unsupported logical scene resource kind",
    ))
}
fn names(shape: snapshot::Shape) -> impl Iterator<Item = &'static str> {
    schema::properties(shape)
        .iter()
        .map(|p| p.name)
        .chain(NodeProperty::PROPERTIES.iter().map(|p| p.name))
}
impl LogicalReferences for SceneCapture<'_> {
    fn property_name(
        &self,
        heap: &Heap,
        property: PropertyId,
    ) -> Result<Option<&'static str>, TypedDataError> {
        let Ok(id) = self.stage.id(heap, property.host) else {
            return Ok(crate::aside::activity::Activities::resource_property_name(
                heap, property,
            )?);
        };
        Ok(names(self.stage.handles[&id].shape).nth(property.field as usize))
    }
    fn capture(
        &self,
        heap: &Heap,
        value: Value,
    ) -> Option<Result<LogicalReference, TypedDataError>> {
        let stage = self.stage;
        let Ok(id) = stage.id(heap, value) else {
            if let Some(header) = super::super::dialogue::character_resource_header(heap, value) {
                return Some(Ok(header));
            }
            if let Some(header) = self
                .media
                .and_then(|media| media.resource_header(heap, value))
            {
                return Some(header);
            }
            if let Some(header) = super::super::dialogue::speech_resource_header(heap, value) {
                return Some(Ok(header));
            }
            if let Some(header) = curve::header(heap, value) {
                return Some(Ok(header));
            }
            if let Some(header) = random::header(heap, value) {
                return Some(Ok(header));
            }
            return self.activities.resource_header(heap, value);
        };
        let shape = stage.handles[&id].shape;
        let Some(kind) = schema::kind(shape) else {
            return Some(Err(TypedDataError::Contract(
                "this scene node requires the current view factory",
            )));
        };
        Some(
            connections::binding_count(heap, value, shape).map(|bindings| LogicalReference {
                payload: Vec::new(),
                kind: kind.into(),
                version: 1,
                fields: (shape.prefix() + NodeProperty::PROPERTIES.len() + 1) as u32 + bindings,
            }),
        )
    }
    fn capture_field(
        &self,
        heap: &Heap,
        value: Value,
        index: u32,
    ) -> Result<(std::borrow::Cow<'static, str>, Value), TypedDataError> {
        let stage = self.stage;
        if stage.id(heap, value).is_err() {
            if super::super::dialogue::character_resource_header(heap, value).is_some() {
                return super::super::dialogue::character_resource_field(heap, value, index);
            }
            if super::super::media::is_media(heap, value)
                || super::super::media::is_cue(heap, value)
            {
                return self
                    .media
                    .ok_or(TypedDataError::Contract("media capture adapter is missing"))?
                    .resource_field(heap, stage, value, index, self.default_owner);
            }
            if super::super::dialogue::speech_resource_header(heap, value).is_some() {
                return super::super::dialogue::speech_resource_field(heap, value, index);
            }
            if curve::header(heap, value).is_some() {
                return curve::field(heap, value, index);
            }
            if let Ok(state) = aside_std::random_resource_state(heap, value) {
                return random::field(state, index);
            }
            return self
                .activities
                .resource_field(heap, stage, value, index, self.default_owner);
        }
        let id = stage.id(heap, value)?;
        let shape = stage.handles[&id].shape;
        if index as usize == shape.prefix() + NodeProperty::PROPERTIES.len() {
            return Ok(("children".into(), heap.host_payload(value, 1)?));
        }
        if index as usize > shape.prefix() + NodeProperty::PROPERTIES.len() {
            return connections::capture_binding(
                heap,
                value,
                shape,
                index - (shape.prefix() + NodeProperty::PROPERTIES.len() + 1) as u32,
            );
        }
        let name = names(shape)
            .nth(index as usize)
            .ok_or(TypedDataError::Contract("invalid scene resource field"))?;
        let property = PropertyId {
            host: value,
            field: index,
        };
        let raw = heap.host_property(property)?;
        if heap.info(raw).is_ok_and(|info| {
            matches!(
                info.kind,
                ObjectKind::Closure
                    | ObjectKind::BoundBuiltin
                    | ObjectKind::BoundNative
                    | ObjectKind::BoundScript
            )
        }) {
            return Err(TypedDataError::Field {
                path: format!("node.{name}"),
                reason: "callback must be rebuilt by the current view factory".into(),
            });
        }
        let binding = heap.property_binding(property)?;
        if binding.tag() != ValueTag::Null {
            let (source, _) = heap.property_ref(binding)?;
            if heap.is_derived(source.host)? {
                return Err(TypedDataError::Field {
                    path: format!("node.{}", name),
                    reason: "derive must be rebuilt by the current view factory".into(),
                });
            }
        }
        Ok((name.into(), raw))
    }
    fn allocate(
        &self,
        _: &mut Heap,
        _: u32,
        _: &LogicalReference,
    ) -> Result<Value, TypedDataError> {
        Err(TypedDataError::Contract(
            "capture adapter cannot allocate resources",
        ))
    }
    fn connect(&self, _: &mut Heap, _: Value, _: &str, _: Value) -> Result<(), TypedDataError> {
        Err(TypedDataError::Contract(
            "capture adapter cannot connect resources",
        ))
    }
    fn finish(&self, _: &mut Heap, _: Value, _: &mut u32) -> Result<bool, TypedDataError> {
        Err(TypedDataError::Contract(
            "capture adapter cannot finish resources",
        ))
    }
}
impl LogicalReferences for SceneRestore<'_> {
    fn property_index(&self, heap: &Heap, value: Value, name: &str) -> Result<u32, TypedDataError> {
        let stage = self.stage.borrow();
        if stage.id(heap, value).is_err() {
            return match name {
                "ticks" => Ok(0),
                "elapsed" => Ok(1),
                _ => Err(TypedDataError::Contract("unknown activity property")),
            };
        }
        let id = stage.id(heap, value)?;
        names(stage.handles[&id].shape)
            .position(|n| n == name)
            .map(|i| i as u32)
            .ok_or(TypedDataError::Contract(
                "binding source native property was removed",
            ))
    }
    fn capture(&self, _: &Heap, _: Value) -> Option<Result<LogicalReference, TypedDataError>> {
        None
    }
    fn capture_field(
        &self,
        _: &Heap,
        _: Value,
        _: u32,
    ) -> Result<(std::borrow::Cow<'static, str>, Value), TypedDataError> {
        Err(TypedDataError::Contract(
            "restore adapter cannot capture resources",
        ))
    }
    fn allocate(
        &self,
        heap: &mut Heap,
        _: u32,
        reference: &LogicalReference,
    ) -> Result<Value, TypedDataError> {
        if reference.version != 1 {
            return Err(TypedDataError::Contract(
                "unsupported scene resource version",
            ));
        }
        if reference.kind == "std.math.curve" {
            return Ok(aside_std::allocate_curve_resource(heap)?);
        }
        if reference.kind == random::KIND {
            return random::allocate(heap, reference);
        }
        if reference.kind == "reel.speech" {
            return super::super::dialogue::allocate_speech_resource(heap);
        }
        if reference.kind == "reel.character" {
            return Ok(super::super::dialogue::allocate_character_resource(heap)?);
        }
        if reference.kind.starts_with("reel.media.") {
            return self
                .media
                .as_ref()
                .ok_or(TypedDataError::Contract("media restore adapter is missing"))?
                .borrow_mut()
                .resource_allocate(heap, reference);
        }
        if reference.kind.starts_with("reel.action.")
            || reference.kind.starts_with("reel.activity.")
        {
            return self.activities.borrow_mut().resource_allocate(
                heap,
                &self.stage.borrow(),
                self.owner,
                reference,
            );
        }
        self.stage
            .borrow_mut()
            .resource_shell(heap, shape(&reference.kind)?)
            .map_err(|error| TypedDataError::Field {
                path: reference.kind.clone(),
                reason: error.to_string(),
            })
    }
    fn connect(
        &self,
        heap: &mut Heap,
        value: Value,
        name: &str,
        field: Value,
    ) -> Result<(), TypedDataError> {
        let stage = self.stage.borrow();
        if super::super::dialogue::character_resource_header(heap, value).is_some() {
            return Ok(super::super::dialogue::connect_character_resource(
                heap, value, name, field,
            )?);
        }
        if super::super::media::is_media(heap, value) || super::super::media::is_cue(heap, value) {
            return self
                .media
                .as_ref()
                .ok_or(TypedDataError::Contract("media restore adapter is missing"))?
                .borrow_mut()
                .resource_connect(heap, &stage, value, name, field);
        }
        if super::super::dialogue::speech_resource_header(heap, value).is_some() {
            return super::super::dialogue::connect_speech_resource(heap, value, name, field);
        }
        if curve::header(heap, value).is_some() {
            return curve::connect(heap, value, name, field);
        }
        if random::recognizes(heap, value) {
            return random::connect(heap, value, name, field);
        }
        if stage.id(heap, value).is_err() {
            return self
                .activities
                .borrow_mut()
                .resource_connect(heap, &stage, value, name, field);
        }
        let id = stage.id(heap, value)?;
        if name == "children" {
            heap.sequence_len(field)?;
            heap.set_host_payload(value, 1, field)?;
            return Ok(());
        }
        if let Some(name) = name.strip_prefix("binding.") {
            let field_index = names(stage.handles[&id].shape)
                .position(|n| n == name)
                .ok_or(TypedDataError::Contract("unknown bound scene property"))?
                as u32;
            return connections::connect_binding(heap, value, field_index, field);
        }
        let index = names(stage.handles[&id].shape)
            .position(|n| n == name)
            .ok_or(TypedDataError::Contract("unknown scene resource property"))?;
        let fields = heap.host_fields(value)?;
        let key = heap.table_entry(fields, index as u32)?.0;
        heap.table_store(fields, index as u32, key, field)
            .map_err(|error| TypedDataError::Field {
                path: format!("{}.{name}", schema::kind(stage.handles[&id].shape).unwrap()),
                reason: error.to_string(),
            })?;
        Ok(())
    }
    fn finish(
        &self,
        heap: &mut Heap,
        value: Value,
        cursor: &mut u32,
    ) -> Result<bool, TypedDataError> {
        let mut stage = self.stage.borrow_mut();
        let stage = &mut **stage;
        if !connections::finish_step(heap, stage)? {
            return Ok(false);
        }
        if super::super::dialogue::character_resource_header(heap, value).is_some() {
            super::super::dialogue::finish_character_resource(heap, value)?;
            return Ok(true);
        }
        if super::super::media::is_media(heap, value) || super::super::media::is_cue(heap, value) {
            return self
                .media
                .as_ref()
                .ok_or(TypedDataError::Contract("media restore adapter is missing"))?
                .borrow_mut()
                .resource_finish(heap, value);
        }
        if super::super::dialogue::speech_resource_header(heap, value).is_some() {
            super::super::dialogue::finish_speech_resource(heap, value)?;
            return Ok(true);
        }
        if curve::header(heap, value).is_some() {
            return Ok(aside_std::validate_curve_resource_step(
                heap, value, cursor,
            )?);
        }
        if random::recognizes(heap, value) {
            return Ok(true);
        }
        if stage.id(heap, value).is_err() {
            return self
                .activities
                .borrow_mut()
                .resource_finish(heap, stage, self.owner, value, cursor);
        }
        let id = stage.id(heap, value)?;
        if *cursor == 0 {
            let shape = stage.handles[&id].shape;
            let fields = heap.host_fields(value)?;
            let (mut kind, input) = if shape == snapshot::Shape::Input {
                let (kind, declaration) = input::prepare(heap, fields)?;
                (
                    kind,
                    Some(input::initialize(heap, value, fields, declaration)?),
                )
            } else {
                (shape.project(heap, fields, None, self.limit)?, None)
            };
            controls::project_group(heap, value, &mut kind)?;
            text_input::project(heap, value, &mut kind)?;
            let props = read_props(
                heap,
                fields,
                shape.prefix(),
                Value::NULL,
                self.limit,
                property_values::PropertyContext {
                    previous: None,
                    appearance_origin: None,
                    effects: &stage.effect_products,
                    effect_projection: &mut stage.effect_projection,
                },
            )?;
            validate_text_props(&kind, &props)?;
            interaction::publish(
                heap,
                value,
                (shape.prefix() + NodeProperty::PROPERTIES.len()) as u32,
                reel_ui::stage::InteractionState {
                    disabled: !props.enabled,
                    ..Default::default()
                },
            )?;
            let binding = stage.handles.get_mut(&id).unwrap();
            binding.input = input;
            binding.interaction_listening =
                interaction::listening(heap, fields, binding.interaction_offset, Value::NULL)?;
            binding.mask_source = mask_handle(heap, fields, shape.prefix(), Value::NULL)?;
            binding.effect_source = effect_stamp(heap, fields, shape.prefix(), Value::NULL)?;
            binding.radio_option = (shape == snapshot::Shape::Radio)
                .then(|| {
                    heap.table_entry(fields, controls::RadioField::Option.index() as u32)
                        .map(|(_, v)| v)
                })
                .transpose()?;
            binding.image_skin = image_skin::binding(heap, shape, fields)?;
            binding.text_source = if binding.image_skin.is_none()
                && matches!(
                    shape,
                    snapshot::Shape::Text
                        | snapshot::Shape::TextInput
                        | snapshot::Shape::TextArea
                        | snapshot::Shape::Image
                        | snapshot::Shape::Video
                ) {
                Some(heap.handle(heap.table_entry(fields, 0)?.1)?)
            } else {
                None
            };
            binding.text_listening = shape == snapshot::Shape::Text
                && heap.table_entry(fields, 1)?.1.tag() != ValueTag::Null;
            stage
                .scene
                .update(id, kind, props)
                .map_err(AdapterError::from)?;
            *cursor += 1;
            return Ok(false);
        }
        let children = heap.host_payload(value, 1)?;
        let index = *cursor - 1;
        if index == heap.sequence_len(children)? {
            return Ok(true);
        }
        let child = heap.sequence_get(children, index)?;
        let child_id = stage.id(heap, child)?;
        stage
            .scene
            .add(id, child_id, index as usize)
            .map_err(AdapterError::from)?;
        heap.set_host_payload(child, 2, value)?;
        *cursor += 1;
        Ok(false)
    }
}
impl StageBinding {
    pub(in crate::aside) fn resource_property_name(
        &self,
        heap: &Heap,
        property: PropertyId,
    ) -> Result<Option<&'static str>, AdapterError> {
        let Ok(id) = self.id(heap, property.host) else {
            return Ok(None);
        };
        Ok(names(self.handles[&id].shape).nth(property.field as usize))
    }
    pub(in crate::aside) fn resource_property_index(
        &self,
        heap: &Heap,
        value: Value,
        name: &str,
    ) -> Result<u32, AdapterError> {
        let id = self.id(heap, value)?;
        names(self.handles[&id].shape)
            .position(|p| p == name)
            .map(|p| p as u32)
            .ok_or(AdapterError::Input("unknown public scene property"))
    }
    /// The ordinary constructor and logical restore share the same current native
    /// payload and validator. No saved code id or device handle enters this shell.
    pub(super) fn allocate_node_shell(
        &self,
        heap: &mut Heap,
        fields: Value,
        children: Value,
        shape: snapshot::Shape,
        type_name: &str,
    ) -> Result<(NodeId, Value), AdapterError> {
        let id = self.scene.next_identity()?;
        let payload = [
            Value::int(id.0 as i64),
            children,
            Value::NULL,
            Value::NULL,
            Value::int(0),
            Value::NULL,
        ];
        let value = heap.alloc_composable_host(fields, &payload, Some(shape.validator()))?;
        self.types.mark(heap, value, type_name)?;
        Ok((id, value))
    }
    fn resource_shell(
        &mut self,
        heap: &mut Heap,
        shape: snapshot::Shape,
    ) -> Result<Value, AdapterError> {
        let mut values = schema::properties(shape)
            .iter()
            .map(|p| default_value(heap, p.parameter().default.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        values.extend(defaults::layout(heap)?);
        let fields = heap.alloc_table()?;
        for (index, (name, value)) in names(shape).zip(values).enumerate() {
            let key = self.property_key(heap, name)?;
            heap.table_store(fields, index as u32, key, value)?;
        }
        let children = heap.alloc_array(&[])?;
        let type_name = registry::node_type(schema::symbol(shape)).unwrap();
        let (id, value) = self.allocate_node_shell(heap, fields, children, shape, type_name)?;
        let offset = (shape.prefix() + NodeProperty::PROPERTIES.len()) as u32;
        for (i, name) in interaction::FIELDS.into_iter().enumerate() {
            let key = self.property_key(heap, name)?;
            heap.table_store(fields, offset + i as u32, key, Value::bool(false))?;
        }
        heap.observe_host_properties(value, offset, interaction::FIELDS.len() as u32)?;
        if matches!(
            shape,
            snapshot::Shape::TextInput | snapshot::Shape::TextArea
        ) {
            for (i, name) in text_input::METRICS.into_iter().enumerate() {
                let key = self.property_key(heap, name)?;
                heap.table_store(
                    fields,
                    text_input::metrics() + i as u32,
                    key,
                    if i < 2 {
                        Value::int(0)
                    } else {
                        Value::bool(false)
                    },
                )?;
            }
            heap.observe_host_properties(
                value,
                text_input::metrics(),
                text_input::METRICS.len() as u32,
            )?;
        }
        if shape == snapshot::Shape::Scroll {
            for (i, name) in scroll::METRICS.into_iter().enumerate() {
                let key = self.property_key(heap, name)?;
                heap.table_store(
                    fields,
                    offset + interaction::FIELDS.len() as u32 + i as u32,
                    key,
                    Value::float(0.),
                )?;
            }
            heap.observe_host_properties(
                value,
                offset + interaction::FIELDS.len() as u32,
                scroll::METRICS.len() as u32,
            )?;
        }
        for field in schema::writable(shape) {
            heap.require_writable_property(PropertyId { host: value, field })?;
        }
        self.scene.create(
            NodeKind::Stack,
            reel_ui::stage::NodeProps::default(),
            Vec::new(),
        )?;
        self.handles.insert(
            id,
            NodeBinding {
                shape,
                handle: heap.handle(value)?,
                interaction_offset: offset,
                interaction_listening: false,
                fields_version: heap.info(fields)?.content_version,
                projection_dirty: true,
                overrides: None,
                input: None,
                text_history: None,
                text_source: None,
                image_skin: None,
                text_listening: false,
                mask_source: None,
                effect_source: None,
                radio_option: None,
            },
        );
        connections::admit(heap, self, value)?;
        Ok(value)
    }
}
fn default_value(
    heap: &mut Heap,
    value: Option<&aside_vm::bytecode::Const>,
) -> Result<Value, HeapError> {
    use aside_vm::bytecode::Const;
    Ok(match value {
        None | Some(Const::Null) => Value::NULL,
        Some(Const::Bool(v)) => Value::bool(*v),
        Some(Const::Int(v)) => Value::int(*v),
        Some(Const::FloatBits(v)) => Value::float(f64::from_bits(*v)),
        Some(Const::Utf8(v)) => heap.alloc_string(v)?,
        _ => return Err(HeapError::TypeMismatch),
    })
}

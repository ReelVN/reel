use super::*;
use crate::bytecode::{ClassDesc, Descriptor, FunctionId};
use crate::class::{ClassSlot, InstanceSlot, MemberSlot, State};
use crate::{CellContract, ObjectKind, ValueTag};

fn descriptor(heap: &Heap, class: Value) -> Result<&ClassDesc, SchemaError> {
    let function = heap
        .get_field(class, ClassSlot::Function as u32)?
        .as_int()
        .unwrap() as usize;
    let index = heap
        .get_field(class, ClassSlot::Descriptor as u32)?
        .as_int()
        .unwrap() as usize;
    let code = heap
        .code
        .as_ref()
        .ok_or(SchemaError::Contract("State class requires current code"))?;
    let Descriptor::Class(desc) = &code.package().functions[function].descriptors[index] else {
        unreachable!()
    };
    Ok(desc)
}
fn source_key(heap: &Heap, class: Value) -> Result<TypeKey, SchemaError> {
    let ty = descriptor(heap, class)?
        .nominal
        .ok_or(SchemaError::Contract(
            "anonymous classes require a stable named State declaration",
        ))?;
    let p = heap.code.as_ref().unwrap().package();
    let DataType::State(key) = data_type(p, ty)? else {
        return Err(SchemaError::Contract("State class has no nominal identity"));
    };
    Ok(key)
}
fn layout(heap: &Heap, class: Value) -> Result<Value, SchemaError> {
    Ok(heap.get_field(class, ClassSlot::Fields as u32)?)
}

impl Registry {
    /// Root the current class and begin incremental schema/default registration.
    /// Explicit type and field identities were supplied when creating this registry.
    pub fn begin_class(&mut self, heap: &mut Heap, class: Value) -> Result<u32, SchemaError> {
        if heap.info(class)?.kind != ObjectKind::Class
            || heap.get_field(class, ClassSlot::State as u32)?.as_int() != Some(State::Ready as i64)
        {
            return Err(SchemaError::Contract(
                "State registration requires a completed current class",
            ));
        }
        let key = self.identities.ty(&source_key(heap, class)?);
        let values = heap.root_value(self.root)?;
        if let Some(&index) = self.by_type.get(&key) {
            if heap.sequence_get(values, index)?.same_bits(class) {
                return Ok(index);
            }
            return Err(SchemaError::Contract(
                "multiple current classes reuse one persistent type key",
            ));
        }
        self.grow(4 * size_of::<BoundClass>() + 256 + 2 * (key.module.len() + key.name.len()))?;
        self.classes
            .try_reserve(1)
            .map_err(|_| SchemaError::Allocation)?;
        let index = u32::try_from(self.classes.len()).map_err(|_| SchemaError::Allocation)?;
        heap.array_push(values, class)?;
        self.by_type.insert(key.clone(), index);
        self.by_handle.insert(heap.handle(class)?.key, index);
        self.classes.push(BoundClass {
            schema: StateSchema {
                key,
                fields: Vec::new(),
            },
            fields: Vec::new(),
            next: 0,
            ready: false,
            pending: None,
            keys: BTreeMap::new(),
        });
        Ok(index)
    }
    /// Advance by one field, recipe instruction or literal-container edge per unit.
    /// Registration failure leaves its class rooted until `release` is called.
    pub fn step_class(&mut self, heap: &Heap, index: u32, work: u32) -> Result<bool, SchemaError> {
        for _ in 0..work {
            let class = heap.sequence_get(heap.root_value(self.root)?, index)?;
            if self.classes[index as usize].ready {
                return Ok(true);
            }
            if let Some(proof) = &mut self.classes[index as usize].pending {
                match proof.step(heap) {
                    Ok(false) => continue,
                    Ok(true) => {
                        let proof = self.classes[index as usize].pending.take().unwrap();
                        self.classes[index as usize]
                            .fields
                            .last_mut()
                            .unwrap()
                            .default = Some(proof.finish()?);
                    }
                    Err(SchemaError::Contract(reason)) => {
                        self.classes[index as usize].pending = None;
                        self.classes[index as usize]
                            .fields
                            .last_mut()
                            .unwrap()
                            .default_error = Some(reason);
                    }
                    Err(e) => return Err(e),
                }
                continue;
            }
            self.grow(512)?;
            let next = self.classes[index as usize].next;
            let fields = layout(heap, class)?;
            if next == heap.sequence_len(fields)? {
                self.classes[index as usize].ready = true;
                return Ok(true);
            }
            let member = heap.get_field(fields, next)?;
            let owner = heap.get_field(member, MemberSlot::Owner as u32)?;
            let member_index = heap
                .get_field(member, MemberSlot::Index as u32)?
                .as_int()
                .unwrap() as usize;
            let field = &descriptor(heap, owner)?.fields[member_index];
            let code = heap.code.as_ref().unwrap();
            let p = code.package();
            let source = FieldKey {
                owner: source_key(heap, owner)?,
                name: p.symbols[field.name.index()].clone(),
            };
            let key = self.identities.field(&source);
            let ty = self
                .data_type(p, field.ty)
                .map_err(|_| SchemaError::Field {
                    key: key.clone(),
                    reason: "field type cannot be stored as data",
                })?;
            if self.classes[index as usize].keys.contains_key(&key) {
                return Err(SchemaError::Field {
                    key,
                    reason: "multiple fields reuse one persistent key",
                });
            }
            let function = heap
                .get_field(owner, ClassSlot::Function as u32)?
                .as_int()
                .unwrap() as usize;
            let contract = CellContract {
                mutable: field.mutable,
                ty: match p.types[field.ty.index()] {
                    TypeDef::Builtin(k) => k,
                    _ => BuiltinType::Any,
                },
                type_id: Some(field.ty),
                module: p.functions[function].module,
                code_version: p.content_version,
                module_version: p.content_version,
            };
            let initializer: Option<FunctionId> = field.initializer;
            self.grow(
                4 * (size_of::<BoundField>() + size_of::<FieldSchema>())
                    + 2 * (key.owner.module.len() + key.owner.name.len() + key.name.len())
                    + source.name.len()
                    + ty.retained_bytes(),
            )?;
            let bound = &mut self.classes[index as usize];
            bound.keys.insert(key.clone(), next);
            bound
                .schema
                .fields
                .try_reserve(1)
                .map_err(|_| SchemaError::Allocation)?;
            bound
                .fields
                .try_reserve(1)
                .map_err(|_| SchemaError::Allocation)?;
            bound.schema.fields.push(FieldSchema { key, ty });
            bound.fields.push(BoundField {
                contract,
                default: None,
                default_error: None,
            });
            bound.next += 1;
            if let Some(function) = initializer {
                match defaults::Proof::new(heap, function) {
                    Ok(proof) => bound.pending = Some(proof),
                    Err(SchemaError::Contract(reason)) => {
                        bound.fields.last_mut().unwrap().default_error = Some(reason)
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(false)
    }
    /// Identify the registered current class of a completed instance.
    pub fn class_for_instance(&self, heap: &Heap, instance: Value) -> Result<u32, SchemaError> {
        let (class, _) = heap.instance_data(instance)?;
        heap.root_value(self.root)?;
        self.by_handle
            .get(&heap.handle(class)?.key)
            .copied()
            .filter(|&i| self.classes[i as usize].ready)
            .ok_or(SchemaError::Contract(
                "instance class has no persistent State registration",
            ))
    }
    /// Read the current class of an instance for bootstrap registration.
    pub fn instance_class(heap: &Heap, instance: Value) -> Result<Value, SchemaError> {
        Ok(heap.instance_data(instance)?.0)
    }
    /// Match one saved field through the current class's stable-key index.
    /// Removed fields return None; retained keys preserve their data type contract.
    pub fn match_field(&self, class: u32, old: &FieldSchema) -> Result<Option<u32>, SchemaError> {
        let current = self.schema(class)?;
        let field = self.classes[class as usize]
            .keys
            .get(&old.key)
            .map(|&n| n as usize);
        if let Some(index) = field
            && current.fields[index].ty != old.ty
        {
            return Err(SchemaError::Field {
                key: old.key.clone(),
                reason: "persistent field type changed",
            });
        }
        Ok(field.map(|i| i as u32))
    }
    /// Allocate an unpublished identity with the current class, without invoking init.
    pub fn begin_instance(&self, heap: &mut Heap, class: u32) -> Result<Value, SchemaError> {
        self.schema(class)?;
        let value = heap.sequence_get(heap.root_value(self.root)?, class)?;
        let fields = heap.alloc_array(&[])?;
        Ok(heap.alloc_record(
            ObjectKind::Instance,
            &[
                value,
                fields,
                Value::int(State::Constructing as i64),
                Value::NULL,
            ],
        )?)
    }
    /// Allocate one current-layout Cell. The caller roots the instance before yielding.
    pub fn allocate_field(
        &self,
        heap: &mut Heap,
        class: u32,
        instance: Value,
        index: u32,
    ) -> Result<(), SchemaError> {
        let fields = heap.get_field(instance, InstanceSlot::Fields as u32)?;
        if heap.sequence_len(fields)? != index {
            return Err(SchemaError::Contract(
                "State fields must be allocated in current layout order",
            ));
        }
        let cell =
            heap.alloc_language_cell(self.classes[class as usize].fields[index as usize].contract)?;
        heap.array_push(fields, cell)?;
        Ok(())
    }
    /// Initialize one restored field using the current complete language contract.
    pub fn initialize_field(
        &self,
        heap: &mut Heap,
        class: u32,
        instance: Value,
        index: u32,
        value: Value,
    ) -> Result<(), SchemaError> {
        let binding = &self.classes[class as usize].fields[index as usize];
        let code = heap.code.as_ref().unwrap();
        if !code.checks.accepts(
            code.package(),
            heap,
            binding.contract.type_id.unwrap(),
            value,
        )? {
            return Err(SchemaError::Field {
                key: self.classes[class as usize].schema.fields[index as usize]
                    .key
                    .clone(),
                reason: "restored value violates the current field type",
            });
        }
        let cell = heap.get_field(
            heap.get_field(instance, InstanceSlot::Fields as u32)?,
            index,
        )?;
        heap.cell_initialize(cell, value)?;
        Ok(())
    }
    pub fn default(&self, class: u32, index: u32) -> Result<&DefaultPlan, SchemaError> {
        let field = &self.classes[class as usize].fields[index as usize];
        field.default.as_ref().ok_or_else(|| SchemaError::Field {
            key: self.classes[class as usize].schema.fields[index as usize]
                .key
                .clone(),
            reason: field
                .default_error
                .unwrap_or("new field has no pure data default"),
        })
    }
    /// Publish only after every current field has been initialized and checked.
    pub fn finish_instance(
        &self,
        heap: &mut Heap,
        class: u32,
        instance: Value,
        observable: bool,
        cursor: &mut u32,
        work: u32,
    ) -> Result<bool, SchemaError> {
        let fields = heap.get_field(instance, InstanceSlot::Fields as u32)?;
        if heap.sequence_len(fields)? as usize != self.classes[class as usize].fields.len() {
            return Err(SchemaError::Contract(
                "State field allocation is incomplete",
            ));
        }
        for _ in 0..work {
            if *cursor == heap.sequence_len(fields)? {
                heap.set_field(
                    instance,
                    InstanceSlot::State as u32,
                    Value::int(State::Ready as i64),
                )?;
                if observable {
                    heap.observe_model_object(instance)?;
                    heap.observe_model_object(fields)?;
                }
                return Ok(true);
            }
            let cell = heap.get_field(fields, *cursor)?;
            if !heap.cell_info(cell)?.initialized {
                return Err(SchemaError::Contract(
                    "State has an uninitialized restored field",
                ));
            }
            if observable {
                heap.observe_model_object(cell)?;
            }
            *cursor += 1;
        }
        Ok(false)
    }
}
impl Heap {
    /// Whether restored data must retain model observation semantics.
    pub fn data_is_observable(&self, value: Value) -> Result<bool, HeapError> {
        Ok(value.tag() == ValueTag::Object && self.object(self.key(value)?)?.model)
    }
    /// Observe one finalized data object. Its connected graph is processed by the caller.
    pub fn observe_restored_data(&mut self, value: Value) -> Result<(), HeapError> {
        self.observe_model_object(value)
    }
}

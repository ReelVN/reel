//! Class layouts are shared traced records. Building a layout and constructing
//! an instance advance one member at a time on the ordinary budgeted stack.
use super::*;

use crate::class::{ClassSlot, InstanceSlot, MemberSlot, State};

/// Borrowed logical owners for one instruction only. A reactive callback or
/// budget yield drops this plan and resolves again after resumption.
pub(super) struct ClassRead {
    receiver: Value,
    member: Value,
    cell: Option<Value>,
    work: u64,
}

impl ClassRead {
    /// Preserve the existing lookup work bound without resolving the layout twice.
    pub(super) fn work(&self) -> u64 {
        self.work
    }
}

#[cfg(test)]
mod read_tests;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) enum ClassStep {
    Build {
        dest: Reg,
        phase: BuildPhase,
        index: u32,
    },
    Insert {
        dest: Reg,
        next: BuildPhase,
        index: u32,
        position: u32,
    },
    Construct {
        dest: Reg,
        call: DescriptorId,
        count: u32,
        phase: ConstructPhase,
        index: u32,
    },
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) enum BuildPhase {
    Members,
    Fields,
    Types,
    OwnFields,
    Methods,
    Statics,
    StaticResult,
    Finish,
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) enum ConstructPhase {
    Allocate,
    Fields,
    FieldResult,
    Init,
    Validate,
}

impl Vm {
    fn class_slot(&self, class: Value, slot: ClassSlot) -> Result<Value, HeapError> {
        self.heap.get_field(class, slot as u32)
    }
    fn instance_slot(&self, instance: Value, slot: InstanceSlot) -> Result<Value, HeapError> {
        self.heap.get_field(instance, slot as u32)
    }
    fn class_desc(&self, class: Value) -> Result<&ClassDesc, HeapError> {
        let function = self
            .class_slot(class, ClassSlot::Function)?
            .as_int()
            .unwrap() as usize;
        let descriptor = self
            .class_slot(class, ClassSlot::Descriptor)?
            .as_int()
            .unwrap() as usize;
        let Descriptor::Class(desc) =
            &self.program.package().functions[function].descriptors[descriptor]
        else {
            unreachable!()
        };
        Ok(desc)
    }
    fn class_alive(&self, value: Value) -> Result<(), FaultKind> {
        let slot = match self.heap.info(value).map_err(heap_fault)?.kind {
            ObjectKind::Class => ClassSlot::State as u32,
            ObjectKind::Instance => InstanceSlot::State as u32,
            _ => return Err(FaultKind::Type),
        };
        if self
            .heap
            .get_field(value, slot)
            .map_err(heap_fault)?
            .as_int()
            == Some(State::Failed as i64)
        {
            Err(FaultKind::ConstructionFailed)
        } else {
            Ok(())
        }
    }
    pub(super) fn abort_class(&mut self, s: ScriptFrame) -> Result<(), VmError> {
        let Ok(value) = self.value(s.env, 3) else {
            return Ok(());
        };
        let slot = match self.heap.info(value)?.kind {
            ObjectKind::Class => ClassSlot::State as u32,
            ObjectKind::Instance => InstanceSlot::State as u32,
            _ => return Err(VmError::Contract("class work root")),
        };
        if self.heap.get_field(value, slot)?.as_int() == Some(State::Constructing as i64) {
            self.heap
                .set_field(value, slot, Value::int(State::Failed as i64))?;
        }
        self.clear(s.env, 3)?;
        Ok(())
    }
    pub(super) fn script_closure(&self, value: Value) -> Result<Value, VmError> {
        if self.heap.info(value)?.kind == ObjectKind::BoundScript {
            let receiver = self.heap.get_field(value, 1)?;
            if !receiver.same_bits(Value::NULL) {
                self.class_alive(receiver)
                    .map_err(|_| VmError::Heap(HeapError::ConstructionFailed))?;
            }
            Ok(self.heap.get_field(value, 0)?)
        } else {
            Ok(value)
        }
    }
    pub(super) fn script_receiver(&self, value: Value) -> Result<Option<Value>, VmError> {
        if self.heap.info(value)?.kind != ObjectKind::BoundScript {
            return Ok(None);
        }
        let receiver = self.heap.get_field(value, 1)?;
        Ok((!receiver.same_bits(Value::NULL)).then_some(receiver))
    }
    pub(super) fn receiver_prefix(
        &self,
        env: RootRangeToken,
        closure: bool,
        shared_parameters: bool,
    ) -> Result<u32, VmError> {
        if !closure || shared_parameters {
            return Ok(0);
        }
        Ok(u32::from(
            self.script_receiver(self.value(env, 0)?)?.is_some(),
        ))
    }
    pub(super) fn enter_initializer(
        &mut self,
        closure: Value,
        function: FunctionId,
    ) -> Result<(), VmError> {
        if self.init_symbol != Some(self.program.package().functions[function.index()].name)
            || self.heap.info(closure)?.kind != ObjectKind::BoundScript
        {
            return Ok(());
        }
        let Some(instance) = self.script_receiver(closure)? else {
            return Ok(());
        };
        if self.instance_slot(instance, InstanceSlot::State)?.as_int()
            != Some(State::Constructing as i64)
        {
            return Err(HeapError::AlreadyInitialized.into());
        }
        let owner = self.heap.get_field(closure, 2)?;
        let calls = self.instance_slot(instance, InstanceSlot::Initializers)?;
        for index in 0..self.heap.info(calls)?.len {
            if self.heap.get_field(calls, index)?.same_bits(owner) {
                return Err(HeapError::AlreadyInitialized.into());
            }
        }
        self.heap.array_push(calls, owner)?;
        Ok(())
    }
    // Reuse only live bindings; the weak cache retains neither receiver nor class.
    fn bind_script(
        &mut self,
        closure: Value,
        receiver: Value,
        owner: Value,
    ) -> Result<Value, HeapError> {
        let key = method_bindings::MethodKey::script(
            closure.object_parts().unwrap().1,
            receiver.object_parts().map_or(0, |(_, key)| key),
            owner.object_parts().unwrap().1,
        );
        if let Some(callable) = self.method_bindings.get(key) {
            let value = Value::object(self.heap.id(), callable);
            if self.heap.info(value).is_ok() {
                self.script_binding_hits += 1;
                return Ok(value);
            }
        }
        self.script_binding_misses += 1;
        let value = self
            .heap
            .alloc_record(ObjectKind::BoundScript, &[closure, receiver, owner])?;
        self.method_bindings
            .remember(key, value.object_parts().unwrap().1);
        Ok(value)
    }
    pub(super) fn super_ref(&mut self, s: ScriptFrame) -> Result<Value, FaultKind> {
        let method = self.value(s.env, 0).map_err(vm_fault)?;
        if self.heap.info(method).map_err(heap_fault)?.kind != ObjectKind::BoundScript {
            return Err(FaultKind::Type);
        }
        let receiver = self.heap.get_field(method, 1).map_err(heap_fault)?;
        let owner = self.heap.get_field(method, 2).map_err(heap_fault)?;
        let base = self
            .class_slot(owner, ClassSlot::Base)
            .map_err(heap_fault)?;
        if base.same_bits(Value::NULL) {
            return Err(FaultKind::MissingKey);
        }
        self.heap
            .alloc_record(ObjectKind::Super, &[receiver, base])
            .map_err(heap_fault)
    }
    fn member_find(&self, class: Value, key: i64) -> Result<(Value, u32), HeapError> {
        let members = self.class_slot(class, ClassSlot::Members)?;
        self.member_find_in(members, self.heap.info(members)?.len, key)
    }
    fn member_find_in(
        &self,
        members: Value,
        len: u32,
        key: i64,
    ) -> Result<(Value, u32), HeapError> {
        let mut lo = 0;
        let mut hi = len;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let member = self.heap.get_field(members, mid)?;
            if self
                .heap
                .get_field(member, MemberSlot::Key as u32)?
                .as_int()
                .unwrap()
                < key
            {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let found = if lo < len {
            let member = self.heap.get_field(members, lo)?;
            (self
                .heap
                .get_field(member, MemberSlot::Key as u32)?
                .as_int()
                == Some(key))
            .then_some(member)
        } else {
            None
        };
        Ok((found.unwrap_or(Value::NULL), lo))
    }
    fn member_target(&self, object: Value, name: SymbolId) -> Result<(Value, Value), FaultKind> {
        let (receiver, member, _) = self.member_read_target(object, name, None)?;
        Ok((receiver, member))
    }
    fn member_read_target(
        &self,
        object: Value,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(Value, Value, u64), FaultKind> {
        let (receiver, class, is_static) = match self.heap.info(object).map_err(heap_fault)?.kind {
            ObjectKind::Instance => (
                object,
                self.instance_slot(object, InstanceSlot::Class)
                    .map_err(heap_fault)?,
                false,
            ),
            ObjectKind::Class => (object, object, true),
            ObjectKind::Super => (
                self.heap.get_field(object, 0).map_err(heap_fault)?,
                self.heap.get_field(object, 1).map_err(heap_fault)?,
                false,
            ),
            _ => return Err(FaultKind::Type),
        };
        self.class_alive(receiver)?;
        let members = self
            .class_slot(class, ClassSlot::Members)
            .map_err(heap_fault)?;
        let info = self.heap.info(members).map_err(heap_fault)?;
        let key = i64::from(name.0) * 2 + i64::from(is_static);
        let cached = site.and_then(|(function, pc)| {
            let (entry, true) =
                self.class_member_hints
                    .get(function, pc, members, info.content_version)?
            else {
                return None;
            };
            if entry >= info.len {
                return None;
            }
            // Resolve the current full member generation. A class/instance
            // switch at the same PC changes the static bit even for one name.
            let member = self.heap.get_field(members, entry).ok()?;
            (self
                .heap
                .get_field(member, MemberSlot::Key as u32)
                .ok()?
                .as_int()
                == Some(key))
            .then_some(member)
        });
        let member = if let Some(member) = cached {
            #[cfg(test)]
            self.work_probe
                .member_hint_hits
                .set(self.work_probe.member_hint_hits.get() + 1);
            member
        } else {
            let (member, entry) = self
                .member_find_in(members, info.len, key)
                .map_err(heap_fault)?;
            if !member.same_bits(Value::NULL)
                && let Some((function, pc)) = site
            {
                self.class_member_hints
                    .remember(function, pc, entry, members, info.content_version);
            }
            member
        };
        if member.same_bits(Value::NULL) {
            return Err(FaultKind::MissingKey);
        }
        Ok((
            receiver,
            member,
            12 + u64::from(info.len.checked_ilog2().unwrap_or(0)) * 3,
        ))
    }
    fn member_cell(&self, receiver: Value, member: Value) -> Result<Value, HeapError> {
        let index = self
            .heap
            .get_field(member, MemberSlot::Offset as u32)?
            .as_int()
            .unwrap() as u32;
        let fields = if self.heap.info(receiver)?.kind == ObjectKind::Class {
            let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
            self.class_slot(owner, ClassSlot::Statics)?
        } else {
            self.instance_slot(receiver, InstanceSlot::Fields)?
        };
        self.heap.get_field(fields, index)
    }
    pub(super) fn class_read_plan(
        &self,
        object: Value,
        name: SymbolId,
        site: Option<(FunctionId, Pc)>,
    ) -> Result<(ClassRead, Option<crate::PropertyId>), FaultKind> {
        let (receiver, member, work) = self.member_read_target(object, name, site)?;
        if self
            .heap
            .get_field(member, MemberSlot::Method as u32)
            .map_err(heap_fault)?
            .as_bool()
            == Some(true)
        {
            return Ok((
                ClassRead {
                    receiver,
                    member,
                    cell: None,
                    work,
                },
                None,
            ));
        }
        let cell = self.member_cell(receiver, member).map_err(heap_fault)?;
        let property = (self.heap.reactive_evaluating()
            && self.heap.is_model_cell(cell).map_err(heap_fault)?)
        .then_some(crate::PropertyId {
            host: cell,
            field: 0,
        });
        Ok((
            ClassRead {
                receiver,
                member,
                cell: Some(cell),
                work,
            },
            property,
        ))
    }
    pub(super) fn class_member(
        &mut self,
        object: Value,
        name: SymbolId,
        write: Option<Value>,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        let (receiver, member) = self.member_target(object, name)?;
        let method = self
            .heap
            .get_field(member, MemberSlot::Method as u32)
            .map_err(heap_fault)?
            .as_bool()
            == Some(true);
        if method {
            if write.is_some() {
                return Err(FaultKind::ReadOnly);
            }
            return self.class_method_value(receiver, member, effects);
        }
        let cell = self.member_cell(receiver, member).map_err(heap_fault)?;
        if let Some(value) = write {
            let info = self.heap.cell_info(cell).map_err(heap_fault)?;
            if !self.heap.is_model_cell(cell).map_err(heap_fault)? {
                self.check_cell_type(info.contract, value)?;
            }
            if info.initialized {
                self.heap.cell_write(cell, value).map_err(heap_fault)?;
            } else {
                self.heap.cell_initialize(cell, value).map_err(heap_fault)?;
            }
            Ok(value)
        } else {
            self.class_cell_value(cell)
        }
    }

    fn class_method_value(
        &mut self,
        receiver: Value,
        member: Value,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        let owner = self
            .heap
            .get_field(member, MemberSlot::Owner as u32)
            .map_err(heap_fault)?;
        let index = self
            .heap
            .get_field(member, MemberSlot::Index as u32)
            .map_err(heap_fault)?
            .as_int()
            .unwrap() as u32;
        let closures = self
            .class_slot(owner, ClassSlot::Closures)
            .map_err(heap_fault)?;
        let closure = self.heap.get_field(closures, index).map_err(heap_fault)?;
        let receiver = if self.heap.info(receiver).map_err(heap_fault)?.kind == ObjectKind::Class {
            Value::NULL
        } else {
            receiver
        };
        self.bind_script(closure, receiver, owner)
            .map_err(heap_fault)
    }

    fn class_cell_value(&mut self, cell: Value) -> Result<Value, FaultKind> {
        let info = self.heap.cell_info(cell).map_err(heap_fault)?;
        if !info.initialized {
            return Err(FaultKind::UninitializedField);
        }
        self.heap.observe_read(cell, Some(0)).map_err(heap_fault)?;
        self.heap.cell_read(cell).map_err(heap_fault)
    }

    pub(super) fn execute_class_read(
        &mut self,
        s: &mut ScriptFrame,
        target: ClassRead,
        dest: Reg,
        next: Pc,
    ) -> Result<bool, FaultKind> {
        let value = if let Some(cell) = target.cell {
            self.class_cell_value(cell)?
        } else {
            self.class_method_value(target.receiver, target.member, s.effects)?
        };
        self.write(s.regs, dest, value)?;
        s.pc = next;
        Ok(true)
    }

    pub(super) fn class_property_ref(
        &mut self,
        object: Value,
        key: Value,
        writable: bool,
    ) -> Result<Value, FaultKind> {
        let name = self
            .type_checks
            .symbol(self.program.package(), &self.heap, key)
            .map_err(heap_fault)?
            .ok_or(FaultKind::MissingKey)?;
        let (receiver, member) = self.member_target(object, name)?;
        if self
            .heap
            .get_field(member, MemberSlot::Method as u32)
            .map_err(heap_fault)?
            .as_bool()
            == Some(true)
        {
            return Err(FaultKind::Type);
        }
        let cell = self.member_cell(receiver, member).map_err(heap_fault)?;
        self.heap
            .alloc_property_ref(
                crate::PropertyId {
                    host: cell,
                    field: 0,
                },
                writable,
            )
            .map_err(heap_fault)
    }
    pub(super) fn class_member_cost(&self, object: Value) -> Result<u64, VmError> {
        let class = match self.heap.info(object)?.kind {
            ObjectKind::Instance => self.instance_slot(object, InstanceSlot::Class)?,
            ObjectKind::Super => self.heap.get_field(object, 1)?,
            _ => object,
        };
        let len = self
            .heap
            .info(self.class_slot(class, ClassSlot::Members)?)?
            .len;
        Ok(12 + u64::from(len.checked_ilog2().unwrap_or(0)) * 3)
    }
    pub(super) fn class_write_cost(&self, object: Value, name: SymbolId) -> Result<u64, VmError> {
        let extra = self
            .member_target(object, name)
            .ok()
            .and_then(|(receiver, member)| self.member_cell(receiver, member).ok())
            .and_then(|cell| self.heap.cell_info(cell).ok())
            .and_then(|info| info.contract.type_id)
            .map_or(0, |ty| self.type_checks.extra(ty));
        Ok(extra)
    }
    pub(super) fn initializer_cost(&self, closure: Value) -> Result<(u64, u64), VmError> {
        let function = self.closure_function(self.heap.get_field(closure, 0)?)?;
        if self.init_symbol != Some(self.program.package().functions[function.index()].name) {
            return Ok((0, 0));
        }
        let Some(receiver) = self.script_receiver(closure)? else {
            return Ok((0, 0));
        };
        let calls = self.instance_slot(receiver, InstanceSlot::Initializers)?;
        if calls.same_bits(Value::NULL) {
            return Ok((0, 0));
        }
        let len = u64::from(self.heap.info(calls)?.len);
        Ok((len + 3, Values::growth_bytes(len, 1)))
    }
    pub(super) fn start_class(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        descriptor: DescriptorId,
    ) -> Result<(), FaultKind> {
        let Descriptor::Class(desc) = &self.program.package().functions[s.code.function.index()]
            .descriptors[descriptor.index()]
        else {
            unreachable!()
        };
        let base = desc
            .base
            .map(|r| self.read(s.regs, r))
            .transpose()?
            .unwrap_or(Value::NULL);
        if !base.same_bits(Value::NULL) {
            if !self.is_text(base, ObjectKind::Class) {
                return Err(FaultKind::Type);
            }
            self.class_alive(base)?;
            if self
                .class_slot(base, ClassSlot::State)
                .map_err(heap_fault)?
                .as_int()
                != Some(State::Ready as i64)
            {
                return Err(FaultKind::Uninitialized);
            }
        }
        let class = self
            .heap
            .alloc_record(
                ObjectKind::Class,
                &[
                    base,
                    Value::int(s.code.function.0 as i64),
                    Value::int(descriptor.0 as i64),
                    Value::NULL,
                    Value::NULL,
                    Value::NULL,
                    Value::NULL,
                    Value::NULL,
                    Value::int(State::Constructing as i64),
                ],
            )
            .map_err(heap_fault)?;
        self.put(s.env, 3, class).map_err(vm_fault)?;
        for slot in [
            ClassSlot::Members,
            ClassSlot::Fields,
            ClassSlot::Statics,
            ClassSlot::Closures,
            ClassSlot::Types,
        ] {
            let array = self.heap.alloc_array(&[]).map_err(heap_fault)?;
            self.heap
                .set_field(class, slot as u32, array)
                .map_err(heap_fault)?;
        }
        s.phase = ScriptPhase::Class(ClassStep::Build {
            dest,
            phase: BuildPhase::Members,
            index: 0,
        });
        Ok(())
    }
    pub(super) fn start_construct(
        &mut self,
        s: &mut ScriptFrame,
        class: Value,
        dest: Reg,
        call: DescriptorId,
        count: u32,
    ) -> Result<(), FaultKind> {
        if s.effects.0 & Effects::ALLOCATE == 0 {
            return Err(FaultKind::Effect);
        }
        self.class_alive(class)?;
        if self
            .class_slot(class, ClassSlot::State)
            .map_err(heap_fault)?
            .as_int()
            != Some(State::Ready as i64)
        {
            return Err(FaultKind::Uninitialized);
        }
        let instance = self
            .heap
            .alloc_record(
                ObjectKind::Instance,
                &[
                    class,
                    Value::NULL,
                    Value::int(State::Constructing as i64),
                    Value::NULL,
                ],
            )
            .map_err(heap_fault)?;
        self.put(s.env, 3, instance).map_err(vm_fault)?;
        for slot in [InstanceSlot::Fields, InstanceSlot::Initializers] {
            let array = self.heap.alloc_array(&[]).map_err(heap_fault)?;
            self.heap
                .set_field(instance, slot as u32, array)
                .map_err(heap_fault)?;
        }
        s.phase = ScriptPhase::Class(ClassStep::Construct {
            dest,
            call,
            count,
            phase: ConstructPhase::Allocate,
            index: 0,
        });
        Ok(())
    }
    fn member_closure(
        &mut self,
        s: ScriptFrame,
        class: Value,
        index: u32,
    ) -> Result<Value, VmError> {
        let desc = self.class_desc(class)?;
        let function = if (index as usize) < desc.fields.len() {
            let field = &desc.fields[index as usize];
            field.initializer
        } else {
            Some(desc.methods[index as usize - desc.fields.len()].function)
        };
        let Some(function) = function else {
            return Ok(Value::NULL);
        };
        self.scratch.data.clear();
        self.scratch.data.extend([
            Value::int(function.0 as i64),
            Value::int(s.code.version as i64),
            Value::int(self.program.package().functions[function.index()].module.0 as i64),
        ]);
        let f = &self.program.package().functions[s.code.function.index()];
        // The descriptor belongs to the current class-building frame.
        let Descriptor::Class(desc) = &f.descriptors[self
            .class_slot(class, ClassSlot::Descriptor)?
            .as_int()
            .unwrap() as usize]
        else {
            unreachable!()
        };
        let captures = if (index as usize) < desc.fields.len() {
            &desc.fields[index as usize].captures
        } else {
            &desc.methods[index as usize - desc.fields.len()].captures
        };
        for source in captures {
            let value = match *source {
                CaptureSource::Binding(id) => {
                    self.value(s.regs, f.bindings[id.index()].storage.reg().0)?
                }
                CaptureSource::Capture(id) => self
                    .heap
                    .get_field(self.script_closure(self.value(s.env, 0)?)?, id.0 + 3)?,
            };
            self.scratch.data.push(value);
        }
        let value = self
            .heap
            .alloc_record(ObjectKind::Closure, &self.scratch.data)?;
        self.scratch.data.clear();
        Ok(value)
    }
    fn class_call(
        &mut self,
        s: ScriptFrame,
        closure: Value,
        source: ArgSource,
        count: u32,
    ) -> Result<(), VmError> {
        let function = self.closure_function(closure)?;
        let i = self.index(s.id)?;
        self.active.frames.data[i] = Frame::Script(s);
        self.push_script(
            function,
            Some(closure),
            source,
            count,
            s.owner,
            inherit(s.context, Context::NoWait),
            s.effects,
            ReturnTo::Class {
                frame: s.id,
                call: s.pc,
            },
        )
    }
    pub(super) fn class_step(
        &mut self,
        s: &mut ScriptFrame,
        step: ClassStep,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let root = self.value(s.env, 3)?;
        let class = if self.heap.info(root)?.kind == ObjectKind::Class {
            root
        } else {
            self.instance_slot(root, InstanceSlot::Class)?
        };
        let mut work = 24;
        let class_growth = |slot| -> Result<u64, VmError> {
            let values = self.class_slot(class, slot)?;
            Ok(Values::growth_bytes(self.heap.info(values)?.len.into(), 1))
        };
        let mut bytes = match step {
            ClassStep::Build {
                phase: phase @ (BuildPhase::Members | BuildPhase::Fields),
                index,
                ..
            } => {
                let slot = if matches!(phase, BuildPhase::Members) {
                    ClassSlot::Members
                } else {
                    ClassSlot::Fields
                };
                let base = self.class_slot(class, ClassSlot::Base)?;
                if !base.same_bits(Value::NULL)
                    && index < self.heap.info(self.class_slot(base, slot)?)?.len
                {
                    class_growth(slot)?
                } else {
                    0
                }
            }
            ClassStep::Build {
                phase: BuildPhase::Types,
                index,
                ..
            } if index < self.program.package().types.len().div_ceil(64) as u32 => {
                class_growth(ClassSlot::Types)?
            }
            ClassStep::Construct {
                phase: ConstructPhase::Allocate,
                index,
                ..
            } => {
                if index
                    < self
                        .heap
                        .info(self.class_slot(class, ClassSlot::Fields)?)?
                        .len
                {
                    let fields = self.instance_slot(root, InstanceSlot::Fields)?;
                    Values::growth_bytes(self.heap.info(fields)?.len.into(), 1)
                } else {
                    0
                }
            }
            ClassStep::Construct {
                phase: ConstructPhase::Fields,
                ..
            } => Values::allocation_bytes(3),
            ClassStep::Construct {
                phase: ConstructPhase::Init,
                ..
            } => {
                let calls = self.instance_slot(root, InstanceSlot::Initializers)?;
                Values::allocation_bytes(3)
                    + Values::growth_bytes(self.heap.info(calls)?.len.into(), 1)
            }
            _ => 0,
        };
        if matches!(
            step,
            ClassStep::Build {
                phase: BuildPhase::Types,
                ..
            }
        ) {
            work += 64;
        }
        if let ClassStep::Build {
            phase: BuildPhase::OwnFields | BuildPhase::Methods,
            index,
            ..
        } = step
        {
            let desc = self.class_desc(class)?;
            let captures = match step {
                ClassStep::Build {
                    phase: BuildPhase::OwnFields,
                    ..
                } => desc.fields.get(index as usize).map(|f| {
                    (
                        f.captures.len(),
                        f.initializer.is_some(),
                        Some(f.is_static),
                        f.name,
                        f.is_static,
                    )
                }),
                _ => desc
                    .methods
                    .get(index as usize)
                    .map(|f| (f.captures.len(), true, None, f.name, f.is_static)),
            };
            if let Some((captures, has_closure, field, name, is_static)) = captures {
                work += captures as u64;
                if has_closure {
                    bytes += Values::allocation_bytes(captures as u64 + 3);
                }
                bytes += class_growth(ClassSlot::Closures)? + Values::allocation_bytes(5);
                let member_growth = class_growth(ClassSlot::Members)?;
                if member_growth != 0 {
                    let key = i64::from(name.0) * 2 + i64::from(is_static);
                    work += self.class_member_cost(class)?;
                    if self.member_find(class, key)?.0.same_bits(Value::NULL) {
                        bytes += member_growth;
                    }
                }
                if let Some(is_static) = field {
                    bytes += class_growth(ClassSlot::Statics)?;
                    if !is_static {
                        bytes += class_growth(ClassSlot::Fields)?;
                    }
                }
            }
            work += self.class_member_cost(class)?;
        }
        let call = match step {
            ClassStep::Build {
                phase: BuildPhase::Statics,
                index,
                ..
            } => {
                let desc = self.class_desc(class)?;
                desc.fields
                    .get(index as usize)
                    .filter(|field| field.is_static)
                    .and_then(|field| field.initializer)
                    .map(|function| (function, 0))
            }
            ClassStep::Construct {
                phase: ConstructPhase::Fields,
                index,
                ..
            } => {
                let fields = self.class_slot(class, ClassSlot::Fields)?;
                if index < self.heap.info(fields)?.len {
                    let member = self.heap.get_field(fields, index)?;
                    let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
                    let field = self
                        .heap
                        .get_field(member, MemberSlot::Index as u32)?
                        .as_int()
                        .unwrap() as usize;
                    self.class_desc(owner)?.fields[field]
                        .initializer
                        .map(|function| (function, 0))
                } else {
                    None
                }
            }
            ClassStep::Construct {
                phase: ConstructPhase::Init,
                count,
                ..
            } => {
                work += self.class_member_cost(class)?;
                let member = self
                    .init_symbol
                    .map(|name| self.member_find(class, i64::from(name.0) * 2))
                    .transpose()?
                    .map(|pair| pair.0)
                    .unwrap_or(Value::NULL);
                if member.same_bits(Value::NULL)
                    || self
                        .heap
                        .get_field(member, MemberSlot::Method as u32)?
                        .as_bool()
                        != Some(true)
                {
                    None
                } else {
                    let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
                    let index = self
                        .heap
                        .get_field(member, MemberSlot::Index as u32)?
                        .as_int()
                        .unwrap() as u32;
                    let closure = self
                        .heap
                        .get_field(self.class_slot(owner, ClassSlot::Closures)?, index)?;
                    Some((self.closure_function(closure)?, count))
                }
            }
            _ => None,
        };
        if let Some((function, count)) = call {
            let (call_work, call_bytes) = self.script_push_cost(function, count);
            work += call_work + 4;
            bytes += call_bytes;
        }
        let result_cell = match step {
            ClassStep::Build {
                phase: BuildPhase::StaticResult,
                index,
                ..
            } => Some(
                self.heap
                    .get_field(self.class_slot(class, ClassSlot::Statics)?, index)?,
            ),
            ClassStep::Construct {
                phase: ConstructPhase::FieldResult,
                index,
                ..
            } => Some(
                self.heap
                    .get_field(self.instance_slot(root, InstanceSlot::Fields)?, index)?,
            ),
            _ => None,
        };
        if let Some(cell) = result_cell {
            work += self
                .heap
                .cell_info(cell)?
                .contract
                .type_id
                .map_or(0, |ty| self.type_checks.extra(ty));
        }
        if !meter.take(0, work, bytes) {
            return Ok(false);
        }
        let result = self.class_advance(s, step, root, class);
        if let Err(error) = result {
            self.abort_class(*s)?;
            self.collection_fault(s, language_fault(error)?)?;
        }
        Ok(true)
    }
    fn class_advance(
        &mut self,
        s: &mut ScriptFrame,
        step: ClassStep,
        root: Value,
        class: Value,
    ) -> Result<(), VmError> {
        match step {
            ClassStep::Build { dest, phase, index } => {
                self.build_class_step(s, class, dest, phase, index)
            }
            ClassStep::Insert {
                dest,
                next,
                index,
                position,
            } => {
                let members = self.class_slot(class, ClassSlot::Members)?;
                if position > 0 {
                    let value = self.heap.get_field(members, position)?;
                    let prev = self.heap.get_field(members, position - 1)?;
                    let key = self.heap.get_field(value, 0)?.as_int().unwrap();
                    let prev_key = self.heap.get_field(prev, 0)?.as_int().unwrap();
                    if key < prev_key {
                        self.heap.set_field(members, position - 1, value)?;
                        self.heap.set_field(members, position, prev)?;
                        s.phase = ScriptPhase::Class(ClassStep::Insert {
                            dest,
                            next,
                            index,
                            position: position - 1,
                        });
                        return Ok(());
                    }
                }
                s.phase = ScriptPhase::Class(ClassStep::Build {
                    dest,
                    phase: next,
                    index,
                });
                Ok(())
            }
            ClassStep::Construct {
                dest,
                call,
                count,
                phase,
                index,
            } => self.construct_step(s, root, class, (dest, call, count), phase, index),
        }
    }
    fn build_class_step(
        &mut self,
        s: &mut ScriptFrame,
        class: Value,
        dest: Reg,
        phase: BuildPhase,
        index: u32,
    ) -> Result<(), VmError> {
        let mut next = phase;
        let mut cursor = index + 1;
        match phase {
            BuildPhase::Members | BuildPhase::Fields => {
                let base = self.class_slot(class, ClassSlot::Base)?;
                let slot = if matches!(phase, BuildPhase::Members) {
                    ClassSlot::Members
                } else {
                    ClassSlot::Fields
                };
                let source = if base.same_bits(Value::NULL) {
                    Value::NULL
                } else {
                    self.class_slot(base, slot)?
                };
                if !source.same_bits(Value::NULL) && index < self.heap.info(source)?.len {
                    self.heap.array_push(
                        self.class_slot(class, slot)?,
                        self.heap.get_field(source, index)?,
                    )?;
                } else {
                    cursor = 0;
                    next = if matches!(phase, BuildPhase::Members) {
                        BuildPhase::Fields
                    } else {
                        BuildPhase::Types
                    };
                }
            }
            BuildPhase::Types => {
                if index < self.program.package().types.len().div_ceil(64) as u32 {
                    let base = self.class_slot(class, ClassSlot::Base)?;
                    let inherited = if base.same_bits(Value::NULL) {
                        0
                    } else {
                        self.heap
                            .get_field(self.class_slot(base, ClassSlot::Types)?, index)?
                            .as_int()
                            .unwrap()
                    };
                    let own = if let Some(nominal) = self.class_desc(class)?.nominal {
                        let p = self.program.package();
                        p.types
                            .iter()
                            .skip(index as usize * 64)
                            .take(64)
                            .enumerate()
                            .fold(0, |bits, (bit, ty)| {
                                if *ty == p.types[nominal.index()] {
                                    bits | (1i64 << bit)
                                } else {
                                    bits
                                }
                            })
                    } else {
                        0
                    };
                    self.heap.array_push(
                        self.class_slot(class, ClassSlot::Types)?,
                        Value::int(inherited | own),
                    )?;
                } else {
                    cursor = 0;
                    next = BuildPhase::OwnFields;
                }
            }
            BuildPhase::OwnFields | BuildPhase::Methods => {
                let desc = self.class_desc(class)?;
                let is_method = matches!(phase, BuildPhase::Methods);
                let len = if is_method {
                    desc.methods.len()
                } else {
                    desc.fields.len()
                };
                if index as usize == len {
                    cursor = 0;
                    next = if is_method {
                        BuildPhase::Statics
                    } else {
                        BuildPhase::Methods
                    };
                } else {
                    let (name, is_static, closure_index, ty, mutable) = if is_method {
                        let method = &desc.methods[index as usize];
                        (
                            method.name,
                            method.is_static,
                            index + desc.fields.len() as u32,
                            None,
                            false,
                        )
                    } else {
                        let field = &desc.fields[index as usize];
                        (
                            field.name,
                            field.is_static,
                            index,
                            Some(field.ty),
                            field.mutable,
                        )
                    };
                    let key = i64::from(name.0) * 2 + i64::from(is_static);
                    let (old, position) = self.member_find(class, key)?;
                    if !old.same_bits(Value::NULL)
                        && (!is_method
                            || self
                                .heap
                                .get_field(old, MemberSlot::Method as u32)?
                                .as_bool()
                                != Some(true))
                    {
                        return Err(HeapError::AlreadyInitialized.into());
                    }
                    let closure = self.member_closure(*s, class, closure_index)?;
                    self.heap
                        .array_push(self.class_slot(class, ClassSlot::Closures)?, closure)?;
                    let offset = if is_static {
                        index
                    } else {
                        self.heap
                            .info(self.class_slot(class, ClassSlot::Fields)?)?
                            .len
                    };
                    if !is_method {
                        let cell = if is_static {
                            self.heap.alloc_language_cell(self.contract(
                                ModuleId(s.module.0),
                                ty.unwrap(),
                                mutable,
                            ))?
                        } else {
                            Value::NULL
                        };
                        self.heap
                            .array_push(self.class_slot(class, ClassSlot::Statics)?, cell)?;
                    }
                    let member = self.heap.alloc_tuple(&[
                        Value::int(key),
                        class,
                        Value::int(closure_index as i64),
                        Value::int(offset as i64),
                        Value::bool(is_method),
                    ])?;
                    if !is_method && !is_static {
                        self.heap
                            .array_push(self.class_slot(class, ClassSlot::Fields)?, member)?;
                    }
                    let members = self.class_slot(class, ClassSlot::Members)?;
                    if old.same_bits(Value::NULL) {
                        self.heap.array_push(members, member)?;
                        s.phase = ScriptPhase::Class(ClassStep::Insert {
                            dest,
                            next: phase,
                            index: cursor,
                            position: self.heap.info(members)?.len - 1,
                        });
                        return Ok(());
                    }
                    self.heap.set_field(members, position, member)?;
                }
            }
            BuildPhase::Statics => {
                let desc = self.class_desc(class)?;
                if index as usize == desc.fields.len() {
                    next = BuildPhase::Finish;
                    cursor = 0;
                } else if desc.fields[index as usize].is_static {
                    let closure = self
                        .heap
                        .get_field(self.class_slot(class, ClassSlot::Closures)?, index)?;
                    if closure.same_bits(Value::NULL) {
                        return Err(HeapError::UninitializedField.into());
                    }
                    s.phase = ScriptPhase::Class(ClassStep::Build {
                        dest,
                        phase: BuildPhase::StaticResult,
                        index,
                    });
                    return self.class_call(*s, closure, ArgSource::None, 0);
                }
            }
            BuildPhase::StaticResult => {
                let cell = self
                    .heap
                    .get_field(self.class_slot(class, ClassSlot::Statics)?, index)?;
                let value = self.value(s.env, 2)?;
                self.check_cell_type(self.heap.cell_info(cell)?.contract, value)
                    .map_err(|_| VmError::Heap(HeapError::TypeMismatch))?;
                self.heap.cell_initialize(cell, value)?;
                self.clear(s.env, 2)?;
                next = BuildPhase::Statics;
            }
            BuildPhase::Finish => {
                self.heap.set_field(
                    class,
                    ClassSlot::State as u32,
                    Value::int(State::Ready as i64),
                )?;
                self.put(s.regs, dest.0, class)?;
                self.clear(s.env, 3)?;
                s.pc = s.instruction_end;
                s.phase = ScriptPhase::Body;
                return Ok(());
            }
        }
        s.phase = ScriptPhase::Class(ClassStep::Build {
            dest,
            phase: next,
            index: cursor,
        });
        Ok(())
    }
    fn construct_step(
        &mut self,
        s: &mut ScriptFrame,
        instance: Value,
        class: Value,
        site: (Reg, DescriptorId, u32),
        phase: ConstructPhase,
        index: u32,
    ) -> Result<(), VmError> {
        let (dest, call, count) = site;
        let layout = self.class_slot(class, ClassSlot::Fields)?;
        let fields = self.instance_slot(instance, InstanceSlot::Fields)?;
        let len = self.heap.info(layout)?.len;
        let mut next = phase;
        let mut cursor = index + 1;
        match phase {
            ConstructPhase::Allocate => {
                if index == len {
                    next = ConstructPhase::Fields;
                    cursor = 0;
                } else {
                    let member = self.heap.get_field(layout, index)?;
                    let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
                    let field_index = self
                        .heap
                        .get_field(member, MemberSlot::Index as u32)?
                        .as_int()
                        .unwrap() as usize;
                    let field = &self.class_desc(owner)?.fields[field_index];
                    let function = self
                        .class_slot(owner, ClassSlot::Function)?
                        .as_int()
                        .unwrap();
                    let cell = self.heap.alloc_language_cell(self.contract(
                        self.program.package().functions[function as usize].module,
                        field.ty,
                        field.mutable,
                    ))?;
                    self.heap.array_push(fields, cell)?;
                }
            }
            ConstructPhase::Fields => {
                if index == len {
                    next = ConstructPhase::Init;
                    cursor = 0;
                } else {
                    let member = self.heap.get_field(layout, index)?;
                    let owner = self.heap.get_field(member, MemberSlot::Owner as u32)?;
                    let field_index = self
                        .heap
                        .get_field(member, MemberSlot::Index as u32)?
                        .as_int()
                        .unwrap() as u32;
                    let closure = self
                        .heap
                        .get_field(self.class_slot(owner, ClassSlot::Closures)?, field_index)?;
                    if !closure.same_bits(Value::NULL) {
                        let bound = self.bind_script(closure, instance, owner)?;
                        self.put(s.env, 2, bound)?;
                        s.phase = ScriptPhase::Class(ClassStep::Construct {
                            dest,
                            call,
                            count,
                            phase: ConstructPhase::FieldResult,
                            index,
                        });
                        return self.class_call(*s, bound, ArgSource::None, 0);
                    }
                }
            }
            ConstructPhase::FieldResult => {
                let cell = self.heap.get_field(fields, index)?;
                let value = self.value(s.env, 2)?;
                self.check_cell_type(self.heap.cell_info(cell)?.contract, value)
                    .map_err(|_| VmError::Heap(HeapError::TypeMismatch))?;
                self.heap.cell_initialize(cell, value)?;
                self.clear(s.env, 2)?;
                next = ConstructPhase::Fields;
            }
            ConstructPhase::Init => {
                let bound = self
                    .init_symbol
                    .map(|name| self.class_member(instance, name, None, s.effects))
                    .transpose();
                match bound {
                    Ok(Some(bound)) => {
                        self.put(s.env, 2, bound)?;
                        s.phase = ScriptPhase::Class(ClassStep::Construct {
                            dest,
                            call,
                            count,
                            phase: ConstructPhase::Validate,
                            index: 0,
                        });
                        return self.class_call(
                            *s,
                            bound,
                            ArgSource::Caller {
                                frame: s.id,
                                descriptor: call,
                            },
                            count,
                        );
                    }
                    Ok(None) | Err(FaultKind::MissingKey) if count == 0 => {
                        next = ConstructPhase::Validate;
                        cursor = 0;
                    }
                    _ => return Err(HeapError::TypeMismatch.into()),
                }
            }
            ConstructPhase::Validate => {
                if index == len {
                    self.heap.set_field(
                        instance,
                        InstanceSlot::State as u32,
                        Value::int(State::Ready as i64),
                    )?;
                    self.heap.set_field(
                        instance,
                        InstanceSlot::Initializers as u32,
                        Value::NULL,
                    )?;
                    self.put(s.regs, dest.0, instance)?;
                    self.clear(s.env, 2)?;
                    self.clear(s.env, 3)?;
                    s.pc = s.instruction_end;
                    s.phase = ScriptPhase::Body;
                    return Ok(());
                }
                if !self
                    .heap
                    .cell_info(self.heap.get_field(fields, index)?)?
                    .initialized
                {
                    return Err(HeapError::UninitializedField.into());
                }
            }
        }
        s.phase = ScriptPhase::Class(ClassStep::Construct {
            dest,
            call,
            count,
            phase: next,
            index: cursor,
        });
        Ok(())
    }
}

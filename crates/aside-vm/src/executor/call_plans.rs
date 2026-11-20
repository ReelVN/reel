//! Instruction plans live only inside one admission/execute step.
use super::*;

pub(super) enum InstructionPlan {
    None,
    ClassRead(classes::ClassRead, Option<crate::PropertyId>),
    Callee(PreparedCallee),
    NamedRead(named_reads::NamedRead),
    Constant(constants::ConstantLoad),
    StringLiteral(Result<Option<Value>, FaultKind>),
    TextMatch { branch: Pc, accepted: bool },
}

/// Values remain reachable through the caller's original target register until
/// ordinary callee roots are published. No borrowed address or plan is persisted.
#[derive(Clone, Copy)]
pub(super) struct PreparedCallee {
    pub(super) function: FunctionId,
    closure: Value,
    pub(super) receiver: Option<Value>,
}

impl Vm {
    pub(super) fn prepare_callee(
        &self,
        closure: Value,
        read: native_calls::CalleeRead<'_>,
    ) -> Option<PreparedCallee> {
        #[cfg(test)]
        if self.work_probe.general_callees {
            return None;
        }
        let function = read.function;
        let f = &self.program.package().functions[function.index()];
        // Initializer lookup, admission and mutation keep their original path.
        if self.init_symbol == Some(f.name) {
            return None;
        }
        let receiver = if matches!(f.kind, FunctionKind::Native(_)) {
            self.callable_receiver_from(closure, read.target).ok()?
        } else {
            read.script_receiver
        };
        // Failed speculative receiver decoding does not escape this function:
        // the original frame/argument quota checks still precede its real error.
        Some(PreparedCallee {
            function,
            closure,
            receiver,
        })
    }

    pub(super) fn execute_call(
        &mut self,
        s: &mut ScriptFrame,
        op: Opcode,
        a: u32,
        b: u32,
        next_pc: Pc,
        prepared: Option<PreparedCallee>,
    ) -> Result<bool, FaultKind> {
        let r = |n| Reg(n);
        let f = &self.program.package().functions[s.code.function.index()];
        let Descriptor::Call(desc) = &f.descriptors[b as usize] else {
            unreachable!()
        };
        let target = desc.target;
        let count = match desc.arguments {
            Arguments::Window(w) => w.len,
            Arguments::Packed(r) => {
                let v = self.read(s.regs, r)?;
                self.heap.packed_array_len(v).map_err(heap_fault)? / 2
            }
        };
        let source = if let Some(contract) = desc.contract {
            ArgSource::CheckedCaller {
                frame: s.id,
                descriptor: DescriptorId(b),
                contract,
            }
        } else {
            ArgSource::Caller {
                frame: s.id,
                descriptor: DescriptorId(b),
            }
        };
        if let Some(contract) = desc.contract {
            let TypeDef::Function { parameters, .. } =
                &self.program.package().types[contract.index()]
            else {
                unreachable!()
            };
            match (count as usize).cmp(&parameters.len()) {
                std::cmp::Ordering::Less => return Err(FaultKind::MissingArgument),
                std::cmp::Ordering::Greater => return Err(FaultKind::UnknownArgument),
                std::cmp::Ordering::Equal => {}
            }
        }
        let call_pc = s.pc;
        let mut context = s.context;
        if op == Opcode::CallNoWait || self.active.unwinds.data.iter().any(|u| u.frame == s.id) {
            context = inherit(context, Context::NoWait);
        }
        let dynamic = if let Some(callee) = prepared {
            Some((callee.closure, callee.function))
        } else if let CallTarget::Register(reg) = target {
            let closure = self.read(s.regs, reg)?;
            if self.is_text(closure, ObjectKind::Class) {
                self.start_construct(s, closure, r(a), DescriptorId(b), count)?;
                return Ok(false);
            }
            if self.is_text(closure, ObjectKind::BoundBuiltin) {
                self.start_builtin(s, closure, DescriptorId(b), r(a), count)?;
                return Ok(false);
            }
            Some((
                closure,
                self.closure_function(closure).map_err(|error| {
                    if matches!(error, VmError::Heap(HeapError::ConstructionFailed)) {
                        FaultKind::ConstructionFailed
                    } else {
                        FaultKind::Type
                    }
                })?,
            ))
        } else {
            None
        };
        s.pc = next_pc;
        let caller_index = self.index(s.id).map_err(|_| FaultKind::InvalidState)?;
        self.publish_script_cursor(caller_index, s);
        match target {
            CallTarget::Native(native) => {
                let site = NativeSite {
                    owner: s.owner,
                    module: s.module,
                    context,
                    effects: s.effects,
                    location: self.loc(s.code, call_pc),
                    ret: ReturnTo::Register {
                        frame: s.id,
                        reg: r(a),
                        call: call_pc,
                    },
                };
                if let Err(e) = self.push_native(native, source, count, site) {
                    s.pc = call_pc;
                    return Err(vm_fault(e));
                }
            }
            CallTarget::Function(function) => {
                if let Err(e) = self.push_script(
                    function,
                    None,
                    source,
                    count,
                    s.owner,
                    context,
                    s.effects,
                    ReturnTo::Register {
                        frame: s.id,
                        reg: r(a),
                        call: call_pc,
                    },
                ) {
                    s.pc = call_pc;
                    return Err(vm_fault(e));
                }
            }
            CallTarget::Register(_) => {
                let (closure, function) = dynamic.unwrap();
                if let Err(e) = self.push_script_with_receiver(
                    function,
                    Some(closure),
                    source,
                    count,
                    s.owner,
                    context,
                    s.effects,
                    ReturnTo::Register {
                        frame: s.id,
                        reg: r(a),
                        call: call_pc,
                    },
                    prepared.map(|callee| callee.receiver),
                ) {
                    s.pc = call_pc;
                    return Err(vm_fault(e));
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;

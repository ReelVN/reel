//! Bounded scalar native calls using rooted caller values directly.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct NativeParameters {
    pub(super) needs_cells: bool,
    pub(super) has_defaults: bool,
    pub(super) read_only_leaf: Option<ReadOnlyLeaf>,
    pub(super) standard_derived_commit: bool,
}

impl Vm {
    #[expect(clippy::too_many_arguments, reason = "one admitted Call instruction")]
    pub(super) fn direct_native_leaf(
        &mut self,
        caller: &mut ScriptFrame,
        destination: Reg,
        descriptor: DescriptorId,
        next: Pc,
        prepared: &InstructionPlan,
        fallback_cost: (u64, u64),
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        if self.legacy_native_leaf_calls {
            return Ok(false);
        }
        if self.active.frames.data.len() == self.limits.max_frames as usize {
            return Ok(false);
        }
        let package = self.program.package();
        let Descriptor::Call(call) =
            &package.functions[caller.code.function.index()].descriptors[descriptor.index()]
        else {
            unreachable!()
        };
        let Arguments::Window(window) = call.arguments else {
            return Ok(false);
        };
        if call.contract.is_some() || window.len > 2 || window.len > self.limits.max_arguments {
            return Ok(false);
        }
        let (native, receiver) = match call.target {
            CallTarget::Native(native) => (native, None),
            CallTarget::Function(function) => {
                let function = &package.functions[function.index()];
                let FunctionKind::Native(native) = function.kind else {
                    return Ok(false);
                };
                if self.init_symbol == Some(function.name) {
                    return Ok(false);
                }
                (native, None)
            }
            CallTarget::Register(_) => {
                let InstructionPlan::Callee(callee) = prepared else {
                    return Ok(false);
                };
                let FunctionKind::Native(native) = package.functions[callee.function.index()].kind
                else {
                    return Ok(false);
                };
                (native, callee.receiver)
            }
        };
        let parameters = self.native_parameters.data[native.index()];
        let Some(leaf) = parameters.read_only_leaf else {
            return Ok(false);
        };
        if parameters.needs_cells || parameters.has_defaults {
            return Ok(false);
        }
        let signature_id = package.natives[native.index()].signature;
        let signature = &package.signatures[signature_id.index()];
        let count = window.len as usize + usize::from(receiver.is_some());
        if count > 2
            || signature.parameters.len() != count
            || signature.rest.is_some()
            || signature
                .parameters
                .iter()
                .any(|p| p.mutable || !matches!(package.types[p.ty.index()], TypeDef::Builtin(_)))
            || !matches!(
                package.types[signature.return_type.index()],
                TypeDef::Builtin(_)
            )
        {
            return Ok(false);
        }
        let permutation = if receiver.is_none() {
            let Some(plan) = self
                .program
                .native_call_plan(caller.code.function, descriptor)
            else {
                return Ok(false);
            };
            self.program.native_call_slots(plan).unwrap()
        } else {
            if !call.names.is_empty() {
                return Ok(false);
            }
            &[]
        };
        let work = u64::from(leaf.work)
            + 4
            + count as u64
            + 2 * self.program.native_call_lookup_work()
            + self.type_checks.extra(signature.return_type)
            + signature
                .parameters
                .iter()
                .map(|p| self.type_checks.extra(p.ty))
                .sum::<u64>();
        // Rejected pure inputs can finish the original Call in this same slice.
        // No new continuation or speculative result survives a budget boundary.
        if !meter.admits(1, work + fallback_cost.0, fallback_cost.1) {
            return Ok(false);
        }
        let supplied =
            match self
                .slots
                .borrow_initialized_window(caller.regs, window.start.0, window.len)
            {
                Ok(values) => values,
                Err(_) => return Ok(false),
            };
        let mut arguments = [Value::NULL; 2];
        let mut positions = [0usize; 2];
        if let Some(receiver) = receiver {
            arguments[0] = receiver;
        }
        for (index, &value) in supplied.iter().enumerate() {
            let position = if receiver.is_some() {
                index + 1
            } else {
                permutation[index] as usize
            };
            arguments[position] = value;
            positions[index + usize::from(receiver.is_some())] = position;
        }
        let types = [
            signature.parameters.first().map(|p| p.ty),
            signature.parameters.get(1).map(|p| p.ty),
        ];
        let result_type = signature.return_type;
        assert!(meter.take(0, work, 0));
        for &position in positions.iter().take(count) {
            if self
                .check_type(types[position].unwrap(), arguments[position])
                .is_err()
            {
                return Ok(false);
            }
        }
        let Some(value) = part_cost!(
            self.step_costs,
            NativeLeafInvoke,
            (leaf.invoke)(&self.heap, &arguments[..count])
        ) else {
            return Ok(false);
        };
        if value.tag() == ValueTag::Object || self.check_type(result_type, value).is_err() {
            return Ok(false);
        }
        assert!(meter.take(1, 0, 0));
        self.put(caller.regs, destination.0, value)?;
        caller.pc = next;
        caller.instruction = None;
        meter.clock_steps = 0;
        Ok(true)
    }
}

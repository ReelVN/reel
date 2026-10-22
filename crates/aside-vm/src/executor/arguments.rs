//! One resumable binder for script/native calls and name lookup for builtins.
//! Dynamic names stay in the argument roots; cursors contain no heap pointers.
use super::*;

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct ArgumentBinding {
    pub index: u32,
    positional: u32,
    named: bool,
    candidate: u32,
    offset: u32,
    resolved: Option<u32>,
}

impl ArgumentBinding {
    pub fn new(positional: u32) -> Self {
        Self {
            positional,
            ..Self::default()
        }
    }
    pub fn advance(&mut self, name: Value) {
        self.index += 1;
        if name.as_int() == Some(-1) {
            self.positional += 1;
        }
        self.candidate = 0;
        self.offset = 0;
        self.resolved = None;
    }
}

pub(super) enum ArgumentStep<T> {
    More,
    Blocked,
    Ready(Result<T, FaultKind>),
}

#[derive(Clone, Copy)]
pub(super) enum ParameterNames {
    Signature(SignatureId),
    Builtin(Builtin),
}

impl ParameterNames {
    fn len(self, program: &VerifiedProgram) -> u32 {
        match self {
            Self::Signature(id) => program.package().signatures[id.index()].parameters.len() as u32,
            Self::Builtin(method) => method.parameters().0.len() as u32,
        }
    }
    fn name(self, program: &VerifiedProgram, index: u32) -> &str {
        match self {
            Self::Signature(id) => {
                let parameter =
                    &program.package().signatures[id.index()].parameters[index as usize];
                &program.package().symbols[parameter.name.index()]
            }
            Self::Builtin(method) => method.parameters().0[index as usize].name,
        }
    }
}

impl Vm {
    pub(super) fn argument_position(
        &self,
        name: Value,
        names: ParameterNames,
        binding: &mut ArgumentBinding,
        meter: &mut Meter,
    ) -> Result<ArgumentStep<u32>, VmError> {
        use ArgumentStep::*;
        if let Some(position) = binding.resolved {
            return Ok(Ready(Ok(position)));
        }
        if name.as_int() == Some(-1) {
            if !meter.take(0, 1, 0) {
                return Ok(Blocked);
            }
            if binding.named {
                return Ok(Ready(Err(FaultKind::ArgumentOrder)));
            }
            binding.resolved = Some(binding.positional);
            return Ok(Ready(Ok(binding.positional)));
        }
        binding.named = true;
        let count = names.len(&self.program);
        if let Some(symbol) = name.as_int() {
            if !meter.take(0, u64::from(count) + 1, 0) {
                return Ok(Blocked);
            }
            let position = match names {
                ParameterNames::Signature(id) => self.program.package().signatures[id.index()]
                    .parameters
                    .iter()
                    .position(|p| i64::from(p.name.0) == symbol),
                ParameterNames::Builtin(method) => self
                    .program
                    .package()
                    .symbols
                    .get(symbol as usize)
                    .and_then(|name| method.parameters().0.iter().position(|p| p.name == name)),
            };
            binding.resolved = position.map(|p| p as u32);
            return Ok(Ready(binding.resolved.ok_or(FaultKind::UnknownArgument)));
        }
        if !self.is_text(name, ObjectKind::String) {
            return Ok(Ready(Err(FaultKind::InvalidState)));
        }
        let len = self.heap.info(name)?.len;
        if binding.candidate == count {
            if !meter.take(0, 1, 0) {
                return Ok(Blocked);
            }
            return Ok(Ready(Err(FaultKind::UnknownArgument)));
        }
        let expected = names.name(&self.program, binding.candidate).as_bytes();
        if expected.len() != len as usize {
            if !meter.take(0, 1, 0) {
                return Ok(Blocked);
            }
            binding.candidate += 1;
            binding.offset = 0;
            return Ok(More);
        }
        let count = (len - binding.offset)
            .min(64)
            .min(self.limits.heap.max_mutation_bytes) as usize;
        if !meter.take(0, count as u64 + 1, 0) {
            return Ok(Blocked);
        }
        let mut bytes = [0; 64];
        self.heap
            .read_bytes(name, binding.offset, &mut bytes[..count])?;
        let start = binding.offset as usize;
        if bytes[..count] != expected[start..start + count] {
            binding.candidate += 1;
            binding.offset = 0;
            return Ok(More);
        }
        binding.offset += count as u32;
        if binding.offset < len {
            return Ok(More);
        }
        binding.resolved = Some(binding.candidate);
        Ok(Ready(Ok(binding.candidate)))
    }

    pub(super) fn bind_argument_step(
        &mut self,
        signature: SignatureId,
        bound: RootRangeToken,
        args: RootRangeToken,
        binding: &mut ArgumentBinding,
        meter: &mut Meter,
        record_position: bool,
    ) -> Result<ArgumentStep<()>, VmError> {
        use ArgumentStep::*;
        let name = self.value(args, binding.index * 2)?;
        let parameter = match self.argument_position(
            name,
            ParameterNames::Signature(signature),
            binding,
            meter,
        )? {
            More => return Ok(More),
            Blocked => return Ok(Blocked),
            Ready(Err(fault)) => return Ok(Ready(Err(fault))),
            Ready(Ok(parameter)) => parameter,
        };
        let sig = &self.program.package().signatures[signature.index()];
        let tail = name.as_int() == Some(-1)
            && sig.rest.is_some()
            && parameter as usize >= sig.parameters.len();
        let slot = if tail {
            sig.parameters.len() as u32
        } else {
            parameter
        };
        let bytes = if tail {
            Values::growth_bytes(self.heap.info(self.value(bound, slot)?)?.len.into(), 1)
        } else {
            0
        };
        let extra = if tail {
            0
        } else {
            self.type_checks.argument_extra(signature)
        };
        if !meter.take(0, 3 + extra + u64::from(record_position), bytes) {
            return Ok(Blocked);
        }
        let value = self.value(args, binding.index * 2 + 1)?;
        let result = if tail {
            self.heap
                .array_push(self.value(bound, slot)?, value)
                .map_err(heap_fault)
        } else if let Some(parameter) = sig.parameters.get(parameter as usize) {
            let ty = parameter.ty;
            match self.slots.read(at(bound, slot)) {
                Ok(_) => Err(FaultKind::DuplicateArgument),
                Err(HeapError::UninitializedSlot) => self.check_type(ty, value).and_then(|()| {
                    self.slots
                        .write(&mut self.heap, at(bound, slot), value)
                        .map_err(heap_fault)
                }),
                Err(e) => Err(heap_fault(e)),
            }
        } else {
            Err(FaultKind::UnknownArgument)
        };
        if result.is_ok() {
            if record_position {
                self.put(args, binding.index * 2, Value::int(i64::from(slot)))?;
            }
            binding.advance(name);
        }
        Ok(Ready(result))
    }
}

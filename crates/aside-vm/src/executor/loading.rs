//! Program admission and VM initialization.
use super::*;

impl Vm {
    pub fn load(
        program: VerifiedProgram,
        support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
    ) -> Result<Self, VmError> {
        Self::load_initial(program, support, limits, epoch, true)
    }
    pub(super) fn load_initial(
        program: VerifiedProgram,
        mut support: RuntimeSupport,
        limits: VmLimits,
        epoch: SessionEpoch,
        initialize: bool,
    ) -> Result<Self, VmError> {
        support.opcodes = SUPPORTED_OPCODES.to_vec();
        program.require_runtime(&support)?;
        program.require_text_pool()?;
        let p = program.package();
        if limits.max_frames == 0
            || limits.max_unwinds == 0
            || limits.max_tasks == 0
            || limits
                .max_arguments
                .checked_add(limits.max_continuation_values)
                .and_then(|n| n.checked_add(3))
                .and_then(|n| n.checked_add(limits.max_native_faults))
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits
                .max_unwinds
                .checked_mul(2)
                .is_none_or(|n| n > limits.slots.max_window_len)
            || limits.heap.max_root_ranges < 4
            || limits.max_native_faults == 0
        {
            return Err(VmError::Limit);
        }
        // Load rejects unsupported operand semantics as well as unsupported opcodes.
        for sig in &p.signatures {
            if sig.property_bag_tail.is_some()
                || sig.parameters.len() + usize::from(sig.rest.is_some())
                    > limits.max_arguments as usize
            {
                return Err(VmError::Contract(
                    "property bag or excessive signature is not supported",
                ));
            }
        }
        for f in &p.functions {
            if !matches!(
                f.kind,
                FunctionKind::Plain | FunctionKind::Generator | FunctionKind::Native(_)
            ) {
                return Err(VmError::Contract("async handler is not connected"));
            }
            if f.register_count > limits.slots.max_window_len
                || f.regions.len() as u64 * 2 + 4 + u64::from(f.kind == FunctionKind::Generator)
                    > limits.slots.max_window_len as u64
            {
                return Err(VmError::Limit);
            }
        }
        let mut heap = Heap::new(epoch, limits.heap)?;
        let usage = heap.budget_usage();
        let type_checks = types::TypeChecks::new(p, &usage, &support.host_types)?;
        let program_charge = Reservation::new(
            &usage,
            program.execution_capacity_bytes() as u64
                + (std::mem::size_of::<CodeLease>() + program.shared_header_bytes()
                    - std::mem::size_of::<BytecodePackage>()
                    + std::mem::size_of::<types::TypeChecks>()
                    + 4 * std::mem::size_of::<usize>()) as u64,
        )?;
        let native_count = p
            .functions
            .iter()
            .filter(|f| matches!(f.kind, FunctionKind::Native(_)))
            .count();
        let module_slots = p.modules.iter().fold(
            p.symbols.len().checked_add(native_count),
            |total, module| total.and_then(|n| n.checked_add(module.globals.len())),
        );
        let module_slots =
            u32::try_from(module_slots.ok_or(VmError::Limit)?).map_err(|_| VmError::Limit)?;
        let execution = limits.slots;
        if execution.initial_capacity == 0
            || execution.initial_capacity > execution.max_capacity
            || execution.max_window_len == 0
            || execution.max_window_len > execution.max_capacity
        {
            return Err(VmError::Limit);
        }
        // Module roots are permanent for this package. Reserving their actual
        // size keeps story length from consuming the execution working set.
        let mut slots = FrameSlots::new(
            &mut heap,
            FrameSlotsLimits {
                initial_capacity: execution
                    .initial_capacity
                    .checked_add(module_slots)
                    .ok_or(VmError::Limit)?,
                max_capacity: execution.max_capacity.saturating_add(module_slots),
                max_window_len: execution.max_window_len,
            },
        )?;
        let control_bytes = std::mem::size_of::<Self>()
            - std::mem::size_of::<Heap>()
            - std::mem::size_of::<FrameSlots>();
        let control_charge = Reservation::new(&usage, control_bytes as u64)?;
        // Derive immutable binder properties once; calls read charged metadata.
        let mut native_parameters = ChargedVec::new(&usage, p.natives.len())?;
        for (index, native) in p.natives.iter().enumerate() {
            native_parameters.data.push(NativeParameters {
                needs_cells: program.native_parameters_need_cells(NativeId(index as u32))?,
                has_defaults: p.signatures[native.signature.index()]
                    .parameters
                    .iter()
                    .any(|p| p.default.is_some()),
            });
        }
        let mut native_values = ChargedVec::new(&usage, native_count)?;
        // Non-owning full-generation handles: reuse live literals without
        // making the entire story a permanent GC root.
        let mut constant_cache = ChargedVec::new(&usage, p.constants.len())?;
        constant_cache.data.resize(p.constants.len(), Value::NULL);
        let active = TaskState::new(&usage, &mut heap, &mut slots, limits, false)?;
        let mut tasks = ChargedVec::new(&usage, 1)?;
        tasks.data.push(TaskState::new(
            &usage, &mut heap, &mut slots, limits, initialize,
        )?);
        let scratch = ChargedVec::new(
            &usage,
            limits.max_arguments.max(
                p.functions
                    .iter()
                    .map(|f| f.captures.len() as u32 + 3)
                    .max()
                    .unwrap_or(3),
            ) as usize,
        )?;
        let text_scratch = ChargedVec::new(
            &usage,
            (limits.heap.max_mutation_bytes as usize).min(BYTE_CHUNK_LEN),
        )?;
        let constants = slots.open_module_set(
            &mut heap,
            RootOwner(u64::MAX),
            if !initialize {
                0
            } else {
                u32::try_from(
                    p.symbols
                        .len()
                        .checked_add(native_count)
                        .ok_or(VmError::Limit)?,
                )
                .map_err(|_| VmError::Limit)?
            },
        )?;
        if initialize {
            // Only member operands need a runtime string. Other symbols remain
            // metadata, avoiding an object allocation per unrelated identifier.
            for &symbol in program.member_symbols() {
                let value = heap.alloc_string(&p.symbols[symbol.index()])?;
                slots.write(&mut heap, at(constants, symbol.0), value)?;
            }
        }
        // Native function values are immutable code constants. Keep one rooted
        // closure per function so reading a native name has stable identity and
        // neither allocates nor adds a synthetic author-visible global.
        for (index, function) in p.functions.iter().enumerate() {
            if matches!(function.kind, FunctionKind::Native(_)) {
                let position = (p.symbols.len() + native_values.data.len()) as u32;
                if initialize {
                    let value = heap.alloc_record(
                        ObjectKind::Closure,
                        &[
                            Value::int(index as i64),
                            Value::int(p.content_version as i64),
                            Value::int(function.module.0 as i64),
                        ],
                    )?;
                    slots.write(&mut heap, at(constants, position), value)?;
                }
                native_values
                    .data
                    .push((FunctionId(index as u32), position));
            }
        }
        let globals = slots.open_module_set(
            &mut heap,
            RootOwner(u64::MAX),
            if initialize {
                p.modules.iter().map(|m| m.globals.len() as u32).sum()
            } else {
                0
            },
        )?;
        if initialize {
            let mut offset = 0;
            for (mi, module) in p.modules.iter().enumerate() {
                for global in &module.globals {
                    let ty = match p.types[global.ty.index()] {
                        TypeDef::Builtin(ty) => ty,
                        _ => BuiltinType::Any,
                    };
                    let cell = heap.alloc_language_cell(CellContract {
                        mutable: global.mutable,
                        ty,
                        type_id: Some(global.ty),
                        module: ModuleId(mi as u32),
                        code_version: p.content_version,
                        module_version: p.content_version,
                    })?;
                    slots.write(&mut heap, at(globals, offset), cell)?;
                    if let Some(f) = global.linked_function {
                        let closure =
                            if matches!(p.functions[f.index()].kind, FunctionKind::Native(_)) {
                                let index = native_values
                                    .data
                                    .binary_search_by_key(&f.0, |(function, _)| function.0)
                                    .expect("prepared native callable");
                                slots.read(at(constants, native_values.data[index].1))?
                            } else {
                                heap.alloc_record(
                                    ObjectKind::Closure,
                                    &[
                                        Value::int(f.0 as i64),
                                        Value::int(p.content_version as i64),
                                        Value::int(mi as i64),
                                    ],
                                )?
                            };
                        if !type_checks.accepts(p, &heap, global.ty, closure)? {
                            return Err(HeapError::TypeMismatch.into());
                        }
                        heap.cell_initialize(cell, closure)?;
                    }
                    offset += 1;
                }
            }
        }
        let init_symbol = p
            .symbols
            .iter()
            .position(|name| name == "init")
            .map(|i| SymbolId(i as u32));
        let type_checks = Rc::new(type_checks);
        let program = Rc::new(CodeLease {
            program,
            charge: program_charge,
            checks: type_checks.clone(),
        });
        heap.code = Some(program.clone());
        Ok(Self {
            program,
            code_window: crate::bytecode::CodeWindow::default(),
            slots,
            active,
            tasks,
            task_index: ChargedVec::new(&usage, 1)?,
            active_slot: None,
            wait_revision: 0,
            scratch,
            text_scratch,
            native_parameters,
            native_values,
            type_checks,
            init_symbol,

            globals,
            constants,
            constant_cache,
            member_hints: member_hints::MemberHints::default(),
            native_bindings: native_bindings::NativeBindings::default(),
            heap,
            limits,
            control_charge,
            next_id: 1,
            native_entries: 0,
            native_binding_hits: 0,
            native_binding_misses: 0,
            native_resumes: 0,
            native_null_publications: 0,
            slices: 0,
            last_usage: SliceUsage::default(),
            #[cfg(test)]
            work_probe: WorkProbe::default(),
        })
    }
}

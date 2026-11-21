//! One control-flow analysis shared by compiler metadata production and verification.
use super::*;
use crate::bytecode::{self as bc, InstructionSequence};

/// Derived current-code continuation layout. Numeric slots never enter saved descriptors.
#[derive(Clone, Debug)]
pub struct SiteLayout {
    pub after_call: Pc,
    pub result: Reg,
    pub live: Vec<BindingId>,
    pub cells: Vec<BindingId>,
    pub initialized_dead: Vec<BindingId>,
    pub work_units: u64,
    pub capacity_bytes: usize,
}
struct Op {
    pc: usize,
    end: usize,
    instruction: bc::Instruction,
    next: Vec<usize>,
    incoming: Option<(usize, usize)>,
    block_entry: usize,
}

/// Shared control-flow and binding facts for every continuation site in one function.
pub struct FlowAnalysis<'a> {
    program: &'a bc::VerifiedProgram,
    function: FunctionId,
    ops: Vec<Op>,
    live: Vec<Vec<bool>>,
    initialized: Vec<Option<Vec<bool>>>,
    cells: Vec<Option<Vec<bool>>>,
    /// Work and temporary storage charged once for this function.
    pub work_units: u64,
    pub capacity_bytes: usize,
}

/// Derive live bindings and initialized lexical cells at a verified call or await boundary.
/// `prepare` is supplied only for a point; return sites resume after the call.
/// Trusted host declarations resolve typed method calls in read-only preparation.
pub fn analyze_site(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    prepare: Option<Pc>,
    call: Pc,
    host_types: &[crate::HostTypeSupport],
    limits: ResumeLimits,
) -> Result<SiteLayout, ResumeError> {
    let mut budget = Budget::new(limits);
    let flow = prepare_flow(program, function, &mut budget).map_err(|e| e.at(function, call))?;
    let mut site = flow.site_in(prepare, call, host_types, &mut budget)?;
    site.work_units = budget.work;
    site.capacity_bytes = budget.bytes;
    Ok(site)
}
pub(crate) fn prepare_flow<'a>(
    program: &'a bc::VerifiedProgram,
    function: FunctionId,
    b: &mut Budget,
) -> Result<FlowAnalysis<'a>, ResumeError> {
    let (start_work, start_bytes) = (b.work, b.bytes);
    let f = program
        .package()
        .functions
        .get(function.index())
        .ok_or_else(|| ResumeError::new("resume function index"))?;
    if f.kind != bc::FunctionKind::Plain || !f.captures.is_empty() {
        return Err(ResumeError::new(
            "resume flow requires a plain function without captures",
        ));
    }
    let registers = f.register_count as usize;
    let count = f.bindings.len();
    admit_scratch(f.code.len(), registers, count, b)?;
    let mut ops = Vec::new();
    reserve(&mut ops, f.code.len())?;
    for item in f.code.ranged_iter() {
        b.work(1)?;
        let (range, instruction) = item.map_err(|e| ResumeError::new(e.to_string()))?;
        ops.push(Op {
            pc: range.start,
            end: range.end,
            instruction,
            next: Vec::new(),
            incoming: None,
            block_entry: 0,
        });
    }
    let mut starts = Vec::new();
    reserve(&mut starts, ops.len())?;
    starts.extend(ops.iter().map(|op| op.pc));
    // A lowered for loop leaves through an iterator-only finally. Modeling
    // Leave as an edge to its destination is valid for lexical binding facts
    // once that cleanup has finished. Arbitrary catch/finally can write bindings.
    for (id, region) in f.regions.iter().enumerate() {
        b.work(u64::from(starts.len().max(1).ilog2()) + 1)?;
        let cleanup = region.finally.filter(|_| region.catch.is_none());
        let valid = cleanup.is_some_and(|range| {
            starts
                .binary_search(&range.start.index())
                .ok()
                .is_some_and(|index| {
                    matches!(ops.get(index..index + 2), Some([close, finish])
                    if close.instruction.opcode == bc::Opcode::IterClose as u16
                    && finish.instruction.opcode == bc::Opcode::EndFinally as u16
                    && finish.instruction.operands[0] as usize == id
                    && finish.end == range.end.index())
                })
        });
        if !valid {
            return Err(ResumeError::new(
                "resume flow cannot analyze a catch or a finally that changes story state",
            ));
        }
    }
    for i in 0..ops.len() {
        b.work(1 + 2 * u64::from(ops.len().checked_ilog2().unwrap_or(0) + 1))?;
        let instruction = ops[i].instruction;
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        let mut targets = Vec::new();
        reserve(&mut targets, 2)?;
        match spec.flow {
            bc::Flow::Next => {
                if i + 1 < ops.len() {
                    targets.push(i + 1);
                }
            }
            bc::Flow::Jump | bc::Flow::Leave => targets.push(
                starts
                    .binary_search(&(instruction.operands[0] as usize))
                    .map_err(|_| ResumeError::new("resume control-flow target"))?,
            ),
            bc::Flow::Branch => {
                for slot in [1, 2] {
                    targets.push(
                        starts
                            .binary_search(&(instruction.operands[slot] as usize))
                            .map_err(|_| ResumeError::new("resume branch target"))?,
                    );
                }
            }
            _ => {}
        }
        for &to in &targets {
            let (low, high) = ops[to].incoming.unwrap_or((i, i));
            ops[to].incoming = Some((low.min(i), high.max(i)));
            if spec.flow != bc::Flow::Next {
                ops[to].block_entry = ops[to].pc;
            }
        }
        ops[i].next = targets;
    }
    b.work(ops.len() as u64)?;
    let mut entry = 0;
    for op in &mut ops {
        entry = entry.max(op.block_entry);
        op.block_entry = entry;
    }
    let mut uses = matrix(ops.len(), registers)?;
    let mut defs = matrix(ops.len(), registers)?;
    for (i, op) in ops.iter().enumerate() {
        let spec = bc::instruction_spec(op.instruction.opcode).unwrap();
        spec.visit_registers_budgeted(
            f,
            &op.instruction,
            |n| {
                b.work(n)
                    .map_err(|e| bc::Error::new(bc::ErrorKind::Limit, e.detail))
            },
            |reg, access| match access {
                bc::Access::Read => uses[i][reg.index()] = true,
                bc::Access::Write => defs[i][reg.index()] = true,
                bc::Access::ReadWrite => {
                    uses[i][reg.index()] = true;
                    defs[i][reg.index()] = true;
                }
                bc::Access::ConditionalWrite => {}
            },
        )
        .map_err(|e| ResumeError::new(e.to_string()))?;
    }
    let mut live = matrix(ops.len(), registers)?;
    loop {
        let mut changed = false;
        for i in (0..ops.len()).rev() {
            b.work(registers as u64 * (1 + ops[i].next.len() as u64))?;
            for r in 0..registers {
                let value =
                    uses[i][r] || (!defs[i][r] && ops[i].next.iter().any(|&to| live[to][r]));
                if value != live[i][r] {
                    live[i][r] = value;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut initialized = filled(ops.len(), None::<Vec<bool>>)?;
    let mut cells = filled(ops.len(), None::<Vec<bool>>)?;
    let mut parameter_init = filled(count, false)?;
    let mut parameter_cells = filled(count, false)?;
    for (index, def) in f.bindings.iter().enumerate() {
        parameter_init[index] =
            matches!(def.initialization, bc::BindingInitialization::Parameter(_));
        parameter_cells[index] =
            parameter_init[index] && matches!(def.storage, bc::BindingStorage::Cell(_));
    }
    initialized[0] = Some(parameter_init);
    cells[0] = Some(parameter_cells);
    loop {
        let mut changed = false;
        for (i, op) in ops.iter().enumerate() {
            let Some(current) = &initialized[i] else {
                continue;
            };
            b.work(count as u64 * 2)?;
            let mut init = copy_bits(current)?;
            let mut allocated = copy_bits(cells[i].as_ref().unwrap())?;
            let instruction = op.instruction;
            match bc::instruction_spec(instruction.opcode).unwrap().opcode {
                bc::Opcode::BindInit => init[instruction.operands[0] as usize] = true,
                bc::Opcode::CellNew => {
                    let at = instruction.operands[0] as usize;
                    allocated[at] = true;
                    init[at] = false;
                }
                bc::Opcode::Clear | bc::Opcode::Clear2 | bc::Opcode::Clear3 => {
                    let n = if instruction.opcode == bc::Opcode::Clear as u16 {
                        1
                    } else if instruction.opcode == bc::Opcode::Clear2 as u16 {
                        2
                    } else {
                        3
                    };
                    for d in 0..count {
                        if instruction.operands[..n].contains(&f.bindings[d].storage.reg().0) {
                            init[d] = false;
                            allocated[d] = false;
                        }
                    }
                }
                _ => {}
            }
            for &to in &op.next {
                b.work(count as u64 * 2)?;
                match &mut initialized[to] {
                    None => {
                        initialized[to] = Some(copy_bits(&init)?);
                        cells[to] = Some(copy_bits(&allocated)?);
                        changed = true;
                    }
                    Some(previous) => {
                        for d in 0..count {
                            let ci = cells[to].as_mut().unwrap();
                            let a = previous[d] && init[d];
                            let c = ci[d] && allocated[d];
                            if a != previous[d] || c != ci[d] {
                                previous[d] = a;
                                ci[d] = c;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    Ok(FlowAnalysis {
        program,
        function,
        ops,
        live,
        initialized,
        cells,
        work_units: b.work - start_work,
        capacity_bytes: b.bytes - start_bytes,
    })
}

impl<'a> FlowAnalysis<'a> {
    /// Analyze the function once and reuse its facts for each declared site.
    pub fn new(
        program: &'a bc::VerifiedProgram,
        function: FunctionId,
        limits: ResumeLimits,
    ) -> Result<Self, ResumeError> {
        prepare_flow(program, function, &mut Budget::new(limits))
    }

    /// Resolve a call using the shared control-flow boundaries. Returns the target and proof work.
    pub fn target(
        &self,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> Result<(FunctionId, u64), ResumeError> {
        let (target, work) = self.target_candidate(call, host_types, limits);
        Ok((target?, work))
    }

    /// Resolve a candidate and report proof work even when it is not eligible.
    pub fn target_candidate(
        &self,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> (Result<FunctionId, ResumeError>, u64) {
        let mut budget = Budget::new(limits);
        let target = self.target_in(call, host_types, &mut budget);
        (target, budget.work)
    }

    pub(super) fn target_in(
        &self,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        budget: &mut Budget,
    ) -> Result<FunctionId, ResumeError> {
        budget.work(u64::from(self.ops.len().max(1).ilog2()) + 1)?;
        let index = self
            .ops
            .binary_search_by_key(&call.index(), |op| op.pc)
            .map_err(|_| ResumeError::new("resume PC is not an instruction boundary"))?;
        super::verify::current_target(
            self.program,
            self.function,
            call,
            host_types,
            budget,
            Some(self.ops[index].block_entry),
        )
        .map_err(|error| error.at(self.function, call))
    }

    /// Check one continuation boundary using the shared function facts.
    pub fn site(
        &self,
        prepare: Option<Pc>,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> Result<SiteLayout, ResumeError> {
        self.site_candidate(prepare, call, host_types, limits).0
    }

    /// Verify a candidate and account for work even when the proof rejects it.
    pub fn site_candidate(
        &self,
        prepare: Option<Pc>,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> (Result<SiteLayout, ResumeError>, u64) {
        let mut budget = Budget::new(limits);
        let site = self.site_in(prepare, call, host_types, &mut budget);
        (site, budget.work)
    }

    pub(crate) fn site_in(
        &self,
        prepare: Option<Pc>,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        b: &mut Budget,
    ) -> Result<SiteLayout, ResumeError> {
        self.check_site(prepare, call, host_types, b)
            .map_err(|error| error.at(self.function, call))
    }

    fn check_site(
        &self,
        prepare: Option<Pc>,
        call: Pc,
        host_types: &[crate::HostTypeSupport],
        b: &mut Budget,
    ) -> Result<SiteLayout, ResumeError> {
        let start_work = b.work;
        let f = &self.program.package().functions[self.function.index()];
        let ops = &self.ops;
        let count = f.bindings.len();
        let registers = f.register_count as usize;
        let locate = |pc: usize| {
            ops.binary_search_by_key(&pc, |op| op.pc)
                .map_err(|_| ResumeError::new("resume PC is not an instruction boundary"))
        };
        b.work(2 * u64::from(ops.len().checked_ilog2().unwrap_or(0) + 1))?;
        let call_index = locate(call.index())?;
        let call_op = ops[call_index].instruction;
        if !matches!(
            bc::instruction_spec(call_op.opcode).unwrap().opcode,
            bc::Opcode::Call | bc::Opcode::CallNoWait | bc::Opcode::Await
        ) {
            return Err(ResumeError::new(
                "resume site must identify a call or await instruction",
            ));
        }
        let entry = prepare.map_or(Ok(call_index + 1), |pc| locate(pc.index()))?;
        if entry >= ops.len() || prepare.is_some_and(|p| p > call) {
            return Err(ResumeError::new("resume entry range"));
        }
        let start = ops[entry.min(call_index)].pc;
        let end = ops[entry.max(call_index)].end;
        for region in &f.regions {
            b.work(1)?;
            let overlaps =
                |range: bc::PcRange| range.start.index() < end && start < range.end.index();
            if overlaps(region.protected) || region.finally.is_some_and(overlaps) {
                return Err(ResumeError::new(
                    "cannot save at this wait inside an active for loop. Move the checkpoint after the loop",
                ));
            }
        }
        if prepare.is_some() {
            for (offset, op) in ops[entry..call_index].iter().enumerate() {
                b.work(1 + op.next.len() as u64)?;
                readonly(self.program, f, op, host_types, b)?;
                if offset != 0
                    && op
                        .incoming
                        .is_some_and(|(low, high)| low < entry || high >= call_index)
                {
                    return Err(ResumeError::new(
                        "point argument preparation has an interior control-flow entry",
                    ));
                }
                if op.next.iter().any(|to| !(entry..=call_index).contains(to)) {
                    return Err(ResumeError::new(
                        "point argument preparation leaves its call",
                    ));
                }
            }
        }
        let live = &self.live[entry];
        let init = self.initialized[entry]
            .as_ref()
            .ok_or_else(|| ResumeError::new("unreachable resume entry"))?;
        let allocated = self.cells[entry].as_ref().unwrap();
        let result = Reg(call_op.operands[0]);
        let mut bindings = Vec::new();
        reserve(&mut bindings, count)?;
        b.work(registers as u64)?;
        for (r, &needed) in live.iter().enumerate() {
            if !needed || (prepare.is_none() && r == result.index()) {
                continue;
            }
            b.work(count as u64)?;
            let Some((id, _)) = f
                .bindings
                .iter()
                .enumerate()
                .find(|(_, d)| d.storage.reg().index() == r)
            else {
                return Err(ResumeError::new(
                    "a temporary value survives the resume entry; use a named business binding",
                ));
            };
            if !init[id] {
                return Err(ResumeError::new(
                    "a live resume binding is not definitely initialized",
                ));
            }
            bindings.push(BindingId(id as u32));
        }
        let mut allocated_cells = Vec::new();
        reserve(&mut allocated_cells, count)?;
        b.work(count as u64 * 3)?;
        for (index, &yes) in allocated.iter().enumerate() {
            if yes {
                allocated_cells.push(BindingId(index as u32));
            }
        }
        let dead = f.bindings.iter().enumerate().filter(|(index, binding)| {
            let reg = binding.storage.reg();
            init[*index]
                && binding.mutable
                && matches!(binding.storage, bc::BindingStorage::Value(_))
                && !live[reg.index()]
                && (prepare.is_some() || reg != result)
        });
        let mut initialized_dead = Vec::new();
        reserve(&mut initialized_dead, dead.clone().count())?;
        initialized_dead.extend(dead.map(|(index, _)| BindingId(index as u32)));
        Ok(SiteLayout {
            after_call: Pc(ops[call_index].end as u32),
            result,
            live: bindings,
            cells: allocated_cells,
            initialized_dead,
            work_units: b.work - start_work,
            capacity_bytes: 0,
        })
    }
}

fn admit_scratch(
    instructions: usize,
    registers: usize,
    bindings: usize,
    b: &mut Budget,
) -> Result<(), ResumeError> {
    use std::mem::size_of;
    let overflow = || ResumeError::new("resume analysis scratch size overflow");
    let rows = registers
        .checked_mul(3)
        .and_then(|n| bindings.checked_mul(4).and_then(|m| n.checked_add(m)))
        .ok_or_else(overflow)?;
    let headers = size_of::<Op>()
        + 3 * size_of::<usize>()
        + 3 * size_of::<Vec<bool>>()
        + 2 * size_of::<Option<Vec<bool>>>();
    let bytes = rows
        .checked_add(headers)
        .and_then(|n| n.checked_mul(instructions))
        .and_then(|n| {
            bindings
                .checked_mul(3 * size_of::<BindingId>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(3 * size_of::<usize>()))
        .ok_or_else(overflow)?;
    // Include all boolean rows and their temporary initialization clones, plus
    // instruction tables, row headers, CFG edges and the returned binding vectors.
    b.bytes(bytes)?;
    let init = registers
        .checked_mul(3)
        .and_then(|n| bindings.checked_mul(2).and_then(|m| n.checked_add(m)))
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(instructions))
        .ok_or_else(overflow)?;
    b.work(u64::try_from(init).map_err(|_| overflow())?)
}
fn reserve<T>(values: &mut Vec<T>, count: usize) -> Result<(), ResumeError> {
    values
        .try_reserve_exact(count)
        .map_err(|_| ResumeError::new("resume analysis scratch allocation"))
}
fn filled<T: Clone>(count: usize, value: T) -> Result<Vec<T>, ResumeError> {
    let mut values = Vec::new();
    reserve(&mut values, count)?;
    values.resize(count, value);
    Ok(values)
}
fn matrix(rows: usize, columns: usize) -> Result<Vec<Vec<bool>>, ResumeError> {
    let mut values = Vec::new();
    reserve(&mut values, rows)?;
    for _ in 0..rows {
        values.push(filled(columns, false)?);
    }
    Ok(values)
}
fn copy_bits(values: &[bool]) -> Result<Vec<bool>, ResumeError> {
    let mut copy = filled(values.len(), false)?;
    for (index, value) in values.iter().enumerate() {
        copy[index] = *value;
    }
    Ok(copy)
}

fn readonly(
    program: &bc::VerifiedProgram,
    f: &bc::RuntimeFunction,
    op: &Op,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
) -> Result<(), ResumeError> {
    let instruction = &op.instruction;
    let spec = bc::instruction_spec(instruction.opcode).unwrap();
    if matches!(spec.opcode, bc::Opcode::Call | bc::Opcode::CallNoWait) {
        let bc::Descriptor::Call(call) = &f.descriptors[instruction.operands[1] as usize] else {
            unreachable!()
        };
        let target = super::targets::static_target(
            program,
            f,
            call.target,
            Pc(op.pc as u32),
            Some(op.block_entry),
            host_types,
            b,
        )?;
        let target = &program.package().functions[target.index()];
        let signature = &program.package().signatures[target.signature.index()];
        if !matches!(target.kind, bc::FunctionKind::Native(_))
            || signature.effects.0 & (bc::Effects::WRITE | bc::Effects::WAIT) != 0
        {
            return Err(ResumeError::new(
                "point arguments require read-only, non-waiting preparation",
            ));
        }
        return Ok(());
    }
    if matches!(
        spec.opcode,
        bc::Opcode::LoadConst
            | bc::Opcode::Move
            | bc::Opcode::Clear
            | bc::Opcode::Clear2
            | bc::Opcode::Clear3
            | bc::Opcode::BindRead
            | bc::Opcode::GlobalRead
            | bc::Opcode::MemberGet
            | bc::Opcode::IndexGet
            | bc::Opcode::PatternField
            | bc::Opcode::CheckType
            | bc::Opcode::Add
            | bc::Opcode::Sub
            | bc::Opcode::Mul
            | bc::Opcode::Div
            | bc::Opcode::Rem
            | bc::Opcode::Eq
            | bc::Opcode::EqNumber
            | bc::Opcode::Ne
            | bc::Opcode::Lt
            | bc::Opcode::Le
            | bc::Opcode::Gt
            | bc::Opcode::Ge
            | bc::Opcode::Not
            | bc::Opcode::Neg
            | bc::Opcode::Pos
            | bc::Opcode::Is
            | bc::Opcode::BitNot
            | bc::Opcode::BitAnd
            | bc::Opcode::BitOr
            | bc::Opcode::BitXor
            | bc::Opcode::Shl
            | bc::Opcode::Shr
            | bc::Opcode::TextNew
            | bc::Opcode::TextPush
            | bc::Opcode::TextFinish
            | bc::Opcode::MakeTuple
            | bc::Opcode::MakeArray
            | bc::Opcode::MakeTable
            | bc::Opcode::Jump
            | bc::Opcode::BranchBool
            | bc::Opcode::BranchNull
    ) {
        Ok(())
    } else {
        Err(ResumeError::new(format!(
            "{} is not supported in point argument preparation",
            spec.mnemonic
        )))
    }
}

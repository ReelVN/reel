//! One control-flow analysis shared by compiler metadata production and verification.
use super::*;
use crate::bytecode::{self as bc, InstructionSequence};
use std::ops::{Index, IndexMut};

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
    next: [usize; 2],
    next_count: usize,
    incoming: Option<(usize, usize)>,
    block_entry: usize,
}
impl Op {
    fn successors(&self) -> &[usize] {
        &self.next[..self.next_count]
    }
}

/// Shared control-flow and binding facts for every continuation site in one function.
pub struct FlowAnalysis<'a> {
    program: &'a bc::VerifiedProgram,
    function: FunctionId,
    ops: Vec<Op>,
    live: Matrix,
    reachable: Vec<bool>,
    initialized: Matrix,
    cells: Matrix,
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
            next: [0; 2],
            next_count: 0,
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
        let mut targets = [0; 2];
        let next_count = match spec.flow {
            bc::Flow::Next if i + 1 < ops.len() => {
                targets[0] = i + 1;
                1
            }
            bc::Flow::Jump | bc::Flow::Leave => {
                targets[0] = starts
                    .binary_search(&(instruction.operands[0] as usize))
                    .map_err(|_| ResumeError::new("resume control-flow target"))?;
                1
            }
            bc::Flow::Branch => {
                for (target, slot) in targets.iter_mut().zip([1, 2]) {
                    *target = starts
                        .binary_search(&(instruction.operands[slot] as usize))
                        .map_err(|_| ResumeError::new("resume branch target"))?;
                }
                2
            }
            _ => 0,
        };
        for &to in &targets[..next_count] {
            let (low, high) = ops[to].incoming.unwrap_or((i, i));
            ops[to].incoming = Some((low.min(i), high.max(i)));
            if spec.flow != bc::Flow::Next {
                ops[to].block_entry = ops[to].pc;
            }
        }
        ops[i].next = targets;
        ops[i].next_count = next_count;
    }
    b.work(ops.len() as u64)?;
    let mut entry = 0;
    for op in &mut ops {
        entry = entry.max(op.block_entry);
        op.block_entry = entry;
    }
    let register_words = registers.div_ceil(usize::BITS as usize);
    let mut uses = Matrix::new(ops.len(), register_words)?;
    let mut defs = Matrix::new(ops.len(), register_words)?;
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
                bc::Access::Read => set_bit(&mut uses[i], reg.index()),
                bc::Access::Write => set_bit(&mut defs[i], reg.index()),
                bc::Access::ReadWrite => {
                    set_bit(&mut uses[i], reg.index());
                    set_bit(&mut defs[i], reg.index());
                }
                bc::Access::ConditionalWrite => {}
            },
        )
        .map_err(|e| ResumeError::new(e.to_string()))?;
    }
    let mut live = Matrix::new(ops.len(), register_words)?;
    loop {
        let mut changed = false;
        for i in (0..ops.len()).rev() {
            let successors = ops[i].successors();
            let mut updated = false;
            b.work(registers as u64 * (1 + successors.len() as u64))?;
            for (word, (&used, &defined)) in uses[i].iter().zip(&defs[i]).enumerate() {
                let live_out = successors.iter().fold(0, |bits, &to| bits | live[to][word]);
                let live_in = used | (live_out & !defined);
                updated |= live[i][word] != live_in;
                live[i][word] = live_in;
            }
            // Earlier predecessors will see these facts later in this sweep.
            changed |= updated && ops[i].incoming.is_some_and(|(_, last)| last >= i);
        }
        if !changed {
            break;
        }
    }
    let binding_words = count.div_ceil(usize::BITS as usize);
    let mut initialized = Matrix::new(ops.len(), binding_words)?;
    let mut cells = Matrix::new(ops.len(), binding_words)?;
    let mut reachable = filled(ops.len(), false)?;
    for (index, def) in f.bindings.iter().enumerate() {
        if matches!(def.initialization, bc::BindingInitialization::Parameter(_)) {
            set_bit(&mut initialized[0], index);
            if matches!(def.storage, bc::BindingStorage::Cell(_)) {
                set_bit(&mut cells[0], index);
            }
        }
    }
    reachable[0] = true;
    let mut init = filled(binding_words, 0usize)?;
    let mut allocated = filled(binding_words, 0usize)?;
    loop {
        let mut changed = false;
        for (i, op) in ops.iter().enumerate() {
            if !reachable[i] {
                continue;
            }
            b.work(count as u64 * 2)?;
            init.copy_from_slice(&initialized[i]);
            allocated.copy_from_slice(&cells[i]);
            let instruction = op.instruction;
            match bc::instruction_spec(instruction.opcode).unwrap().opcode {
                bc::Opcode::BindInit => set_bit(&mut init, instruction.operands[0] as usize),
                bc::Opcode::CellNew => {
                    let at = instruction.operands[0] as usize;
                    set_bit(&mut allocated, at);
                    clear_bit(&mut init, at);
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
                            clear_bit(&mut init, d);
                            clear_bit(&mut allocated, d);
                        }
                    }
                }
                _ => {}
            }
            for &to in op.successors() {
                b.work(count as u64 * 2)?;
                if !reachable[to] {
                    reachable[to] = true;
                    initialized[to].copy_from_slice(&init);
                    cells[to].copy_from_slice(&allocated);
                    changed |= to <= i;
                } else {
                    let previous = &mut initialized[to];
                    let ci = &mut cells[to];
                    for word in 0..binding_words {
                        let a = previous[word] & init[word];
                        let c = ci[word] & allocated[word];
                        changed |= to <= i && (a != previous[word] || c != ci[word]);
                        previous[word] = a;
                        ci[word] = c;
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
        reachable,
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
                b.work(1 + op.successors().len() as u64)?;
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
                if op
                    .successors()
                    .iter()
                    .any(|to| !(entry..=call_index).contains(to))
                {
                    return Err(ResumeError::new(
                        "point argument preparation leaves its call",
                    ));
                }
            }
        }
        let live = &self.live[entry];
        if !self.reachable[entry] {
            return Err(ResumeError::new("unreachable resume entry"));
        }
        let init = &self.initialized[entry];
        let allocated = &self.cells[entry];
        let result = Reg(call_op.operands[0]);
        let mut bindings = Vec::new();
        reserve(&mut bindings, count)?;
        b.work(registers as u64)?;
        for r in 0..registers {
            if !has_bit(live, r) || (prepare.is_none() && r == result.index()) {
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
            if !has_bit(init, id) {
                return Err(ResumeError::new(
                    "a live resume binding is not definitely initialized",
                ));
            }
            bindings.push(BindingId(id as u32));
        }
        let mut allocated_cells = Vec::new();
        reserve(&mut allocated_cells, count)?;
        b.work(count as u64 * 3)?;
        for index in 0..count {
            if has_bit(allocated, index) {
                allocated_cells.push(BindingId(index as u32));
            }
        }
        let dead = f.bindings.iter().enumerate().filter(|(index, binding)| {
            let reg = binding.storage.reg();
            has_bit(init, *index)
                && binding.mutable
                && matches!(binding.storage, bc::BindingStorage::Value(_))
                && !has_bit(live, reg.index())
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
    let binding_words = bindings.div_ceil(usize::BITS as usize);
    let binding_row_bytes = binding_words * size_of::<usize>();
    let rows = registers
        .div_ceil(usize::BITS as usize)
        .checked_mul(3 * size_of::<usize>())
        .and_then(|n| n.checked_add(2 * binding_row_bytes))
        .ok_or_else(overflow)?;
    let headers = size_of::<Op>() + size_of::<usize>() + size_of::<bool>();
    let bytes = rows
        .checked_add(headers)
        .and_then(|n| n.checked_mul(instructions))
        .and_then(|n| {
            bindings
                .checked_mul(3 * size_of::<BindingId>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(2 * binding_row_bytes + 3 * size_of::<usize>()))
        .ok_or_else(overflow)?;
    // Include all fact rows, two reusable scratch rows, and the returned bindings.
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
struct Matrix {
    columns: usize,
    values: Vec<usize>,
}
impl Matrix {
    fn new(rows: usize, columns: usize) -> Result<Self, ResumeError> {
        Ok(Self {
            columns,
            values: filled(rows * columns, 0)?,
        })
    }
}
impl Index<usize> for Matrix {
    type Output = [usize];
    fn index(&self, row: usize) -> &Self::Output {
        let start = row * self.columns;
        &self.values[start..start + self.columns]
    }
}
impl IndexMut<usize> for Matrix {
    fn index_mut(&mut self, row: usize) -> &mut Self::Output {
        let start = row * self.columns;
        &mut self.values[start..start + self.columns]
    }
}
fn set_bit(row: &mut [usize], bit: usize) {
    let width = usize::BITS as usize;
    row[bit / width] |= 1 << (bit % width);
}

fn clear_bit(row: &mut [usize], bit: usize) {
    let width = usize::BITS as usize;
    row[bit / width] &= !(1 << (bit % width));
}

fn has_bit(row: &[usize], bit: usize) -> bool {
    let width = usize::BITS as usize;
    row[bit / width] & (1 << (bit % width)) != 0
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

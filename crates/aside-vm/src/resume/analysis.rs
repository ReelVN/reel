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
    pub work_units: u64,
    pub capacity_bytes: usize,
}
struct Op {
    pc: usize,
    end: usize,
    instruction: bc::Instruction,
    next: Vec<usize>,
}

/// Derive live bindings and initialized lexical cells at a verified call boundary.
/// `prepare` is supplied only for a point; return sites resume after the call.
pub fn analyze_site(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    prepare: Option<Pc>,
    call: Pc,
    limits: ResumeLimits,
) -> Result<SiteLayout, ResumeError> {
    let mut budget = Budget::new(limits);
    analyze(program, function, prepare, call, &mut budget).map_err(|e| e.at(function, call))
}
pub(crate) fn analyze(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    prepare: Option<Pc>,
    call: Pc,
    b: &mut Budget,
) -> Result<SiteLayout, ResumeError> {
    let f = program
        .package()
        .functions
        .get(function.index())
        .ok_or_else(|| ResumeError::new("resume function index"))?;
    if f.kind != bc::FunctionKind::Plain || !f.captures.is_empty() || !f.regions.is_empty() {
        return Err(ResumeError::new(
            "resume flow requires a plain function without captures or exception regions",
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
        });
    }
    let locate = |pc: usize| {
        ops.binary_search_by_key(&pc, |op| op.pc)
            .map_err(|_| ResumeError::new("resume PC is not an instruction boundary"))
    };
    let call_index = locate(call.index())?;
    let call_op = ops[call_index].instruction;
    if !matches!(
        bc::instruction_spec(call_op.opcode).unwrap().opcode,
        bc::Opcode::Call | bc::Opcode::CallNoWait
    ) {
        return Err(ResumeError::new(
            "resume site must identify a call instruction",
        ));
    }
    let entry = prepare.map_or(Ok(call_index + 1), |pc| locate(pc.index()))?;
    if entry >= ops.len() || prepare.is_some_and(|p| p > call) {
        return Err(ResumeError::new("resume entry range"));
    }
    if prepare.is_some() {
        for op in &ops[entry..call_index] {
            b.work(1)?;
            readonly(program, f, &op.instruction, b)?;
        }
    }
    let mut starts = Vec::new();
    reserve(&mut starts, ops.len())?;
    starts.extend(ops.iter().map(|op| op.pc));
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
        ops[i].next = targets;
    }
    if prepare.is_some() {
        for (i, op) in ops.iter().enumerate() {
            for &to in &op.next {
                if (entry..call_index).contains(&to)
                    && !(entry..call_index).contains(&i)
                    && to != entry
                {
                    return Err(ResumeError::new(
                        "point argument preparation has an interior control-flow entry",
                    ));
                }
                if (entry..call_index).contains(&i) && !(entry..=call_index).contains(&to) {
                    return Err(ResumeError::new(
                        "point argument preparation leaves its call",
                    ));
                }
            }
        }
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
    let init = initialized[entry]
        .as_ref()
        .ok_or_else(|| ResumeError::new("unreachable resume entry"))?;
    let allocated = cells[entry].as_ref().unwrap();
    let result = Reg(call_op.operands[0]);
    let mut bindings = Vec::new();
    reserve(&mut bindings, count)?;
    b.work(registers as u64)?;
    for (r, &needed) in live[entry].iter().enumerate() {
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
    b.work(count as u64)?;
    for (index, &yes) in allocated.iter().enumerate() {
        if yes {
            allocated_cells.push(BindingId(index as u32));
        }
    }
    Ok(SiteLayout {
        after_call: Pc(ops[call_index].end as u32),
        result,
        live: bindings,
        cells: allocated_cells,
        work_units: b.work,
        capacity_bytes: b.bytes,
    })
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
                .checked_mul(2 * size_of::<BindingId>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(2 * size_of::<usize>()))
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
    instruction: &bc::Instruction,
    b: &mut Budget,
) -> Result<(), ResumeError> {
    let spec = bc::instruction_spec(instruction.opcode).unwrap();
    if matches!(spec.opcode, bc::Opcode::Call | bc::Opcode::CallNoWait) {
        let bc::Descriptor::Call(call) = &f.descriptors[instruction.operands[1] as usize] else {
            unreachable!()
        };
        let target = super::targets::static_target(program, f, call.target, None, b)?;
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
            | bc::Opcode::CheckType
            | bc::Opcode::Add
            | bc::Opcode::Sub
            | bc::Opcode::Mul
            | bc::Opcode::Div
            | bc::Opcode::Rem
            | bc::Opcode::Eq
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

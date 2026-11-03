//! Verified lifetime suffixes share the scalar executor's original value operation.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScalarSuffix {
    pub(crate) pc: Pc,
    pub(crate) end: Pc,
    pub(crate) last_pc: Pc,
    pub(crate) destination: u32,
    pub(crate) clears: [u32; 4],
    pub(crate) clear_count: u8,
    pub(crate) prefix_count: u8,
    pub(crate) moved: bool,
    pub(crate) dead: bool,
}
impl ScalarSuffix {
    pub(crate) fn instructions(&self) -> u32 {
        1 + u32::from(self.prefix_count != 0) + 2 * u32::from(self.moved)
    }
    pub(crate) fn work(&self) -> u64 {
        1 + u64::from(self.prefix_count) + 2 * u64::from(self.moved)
    }
}

#[derive(Debug)]
pub(super) struct ScalarSuffixes {
    offsets: Vec<u32>,
    group_offsets: Vec<u32>,
    groups: Vec<Group>,
    plans: Vec<ScalarSuffix>,
}
#[derive(Clone, Copy, Debug, Default)]
struct Group {
    bits: u32,
    first: u32,
}
impl ScalarSuffixes {
    pub(super) fn capacity_bytes(&self) -> usize {
        self.offsets.capacity() * size_of::<u32>()
            + self.group_offsets.capacity() * size_of::<u32>()
            + self.groups.capacity() * size_of::<Group>()
            + self.plans.capacity() * size_of::<ScalarSuffix>()
    }
    pub(super) fn rebase(&mut self, function: usize, pcs: &[u32]) {
        let range = self.offsets[function] as usize..self.offsets[function + 1] as usize;
        for plan in &mut self.plans[range] {
            plan.pc = Pc(pcs[plan.pc.index()]);
            plan.end = Pc(pcs[plan.end.index()]);
            plan.last_pc = Pc(pcs[plan.last_pc.index()]);
        }
        self.index(function);
    }
    fn index(&mut self, function: usize) {
        let mut site = self.offsets[function] as usize;
        let end = self.offsets[function + 1] as usize;
        let range =
            self.group_offsets[function] as usize..self.group_offsets[function + 1] as usize;
        for (word, group) in self.groups[range].iter_mut().enumerate() {
            *group = Group {
                bits: 0,
                first: site as u32,
            };
            while site < end && self.plans[site].pc.index() / 32 == word {
                group.bits |= 1 << (self.plans[site].pc.0 % 32);
                site += 1;
            }
        }
        debug_assert_eq!(site, end);
    }
}
impl VerifiedProgram {
    pub(crate) fn scalar_suffix(&self, function: FunctionId, pc: Pc) -> Option<&ScalarSuffix> {
        let suffixes = &self.0.scalar_suffixes;
        let first = suffixes.group_offsets[function.index()] as usize;
        let end = suffixes.group_offsets[function.index() + 1] as usize;
        let group = suffixes.groups[first..end].get(pc.index() / 32)?;
        let bit = 1 << (pc.0 % 32);
        (group.bits & bit != 0).then(|| {
            let rank = (group.bits & (bit - 1)).count_ones();
            &suffixes.plans[(group.first + rank) as usize]
        })
    }
    #[cfg(test)]
    pub(crate) fn scalar_suffix_capacity_bytes(&self) -> usize {
        self.0.scalar_suffixes.capacity_bytes()
    }
}

fn value_opcode(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::LoadConst
            | Opcode::Move
            | Opcode::BindRead
            | Opcode::CellRead
            | Opcode::Neg
            | Opcode::Pos
            | Opcode::Not
            | Opcode::BitNot
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Rem
            | Opcode::BitAnd
            | Opcode::BitOr
            | Opcode::BitXor
            | Opcode::Shl
            | Opcode::Shr
            | Opcode::Lt
            | Opcode::Le
            | Opcode::Gt
            | Opcode::Ge
    )
}
type Decoded = (u32, u32, Instruction);
fn candidate(window: &[Option<Decoded>; 4]) -> Option<ScalarSuffix> {
    let (pc, _, producer) = window[0]?;
    if !value_opcode(instruction_spec(producer.opcode)?.opcode) {
        return None;
    }
    let temporary = producer.operands[0];
    let mut plan = ScalarSuffix {
        pc: Pc(pc),
        end: Pc(pc),
        last_pc: Pc(pc),
        destination: temporary,
        clears: [0; 4],
        clear_count: 0,
        prefix_count: 0,
        moved: false,
        dead: false,
    };
    let mut next = 1;
    if let Some((clear_pc, end, clear)) = window[1] {
        let count = match instruction_spec(clear.opcode)?.opcode {
            Opcode::Clear => 1,
            Opcode::Clear2 => 2,
            Opcode::Clear3 => 3,
            _ => 0,
        };
        if count != 0 {
            plan.clears[..count].copy_from_slice(&clear.operands[..count]);
            plan.clear_count = count as u8;
            plan.prefix_count = count as u8;
            plan.dead = plan.clears[..count].contains(&temporary);
            plan.end = Pc(end);
            plan.last_pc = Pc(clear_pc);
            next += 1;
        }
    }
    if !plan.dead
        && let (Some((_, _, mov)), Some((clear_pc, end, clear))) = (window[next], window[next + 1])
        && mov.opcode == Opcode::Move as u16
        && mov.operands[1] == temporary
        && mov.operands[0] != temporary
        && clear.opcode == Opcode::Clear as u16
        && clear.operands[0] == temporary
    {
        plan.destination = mov.operands[0];
        plan.clears[plan.clear_count as usize] = temporary;
        plan.clear_count += 1;
        plan.moved = true;
        plan.end = Pc(end);
        plan.last_pc = Pc(clear_pc);
    }
    (plan.clear_count != 0).then_some(plan)
}
fn scan(code: &impl InstructionSequence, mut emit: impl FnMut(ScalarSuffix)) -> Result<(), Error> {
    let mut iter = code.ranged_iter();
    let mut window = [None; 4];
    for slot in &mut window {
        *slot = iter
            .next()
            .transpose()?
            .map(|(r, i)| (r.start as u32, r.end as u32, i));
    }
    while window[0].is_some() {
        if let Some(plan) = candidate(&window) {
            emit(plan);
        }
        window.copy_within(1.., 0);
        window[3] = iter
            .next()
            .transpose()?
            .map(|(r, i)| (r.start as u32, r.end as u32, i));
    }
    Ok(())
}
pub(super) fn build(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    b: &mut Budget,
) -> Result<ScalarSuffixes, Error> {
    let mut offsets = b.vec(p.functions.len() + 1)?;
    let mut group_offsets = b.vec(p.functions.len() + 1)?;
    let mut count = 0;
    let mut group_count = 0;
    offsets.push(0);
    group_offsets.push(0);
    for f in &p.functions {
        b.work(f.code.len() as u64 * 12)?;
        let before = count;
        scan(&f.code, |_| count += 1)?;
        offsets.push(
            u32::try_from(count)
                .map_err(|_| Error::new(ErrorKind::Limit, "scalar suffix count"))?,
        );
        if count != before {
            let words = f.code.iter().try_fold(0usize, |n, i| {
                n.checked_add(1 + CompactCode::shape(&i?)?.3)
                    .ok_or_else(|| Error::new(ErrorKind::Limit, "scalar suffix word count"))
            })?;
            group_count += words.div_ceil(32);
        }
        group_offsets.push(
            u32::try_from(group_count)
                .map_err(|_| Error::new(ErrorKind::Limit, "scalar suffix index"))?,
        );
    }
    let mut groups = b.vec(group_count)?;
    groups.resize(group_count, Group::default());
    let mut plans = b.vec(count)?;
    for f in &p.functions {
        scan(&f.code, |plan| plans.push(plan))?;
    }
    let mut result = ScalarSuffixes {
        offsets,
        group_offsets,
        groups,
        plans,
    };
    for function in 0..p.functions.len() {
        result.index(function);
    }
    Ok(result)
}

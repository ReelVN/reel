//! Instruction edges and exception region verification.
use super::*;

pub(super) fn ranges(r: &ExceptionRegion) -> impl Iterator<Item = PcRange> + '_ {
    std::iter::once(r.protected)
        .chain(r.catch.as_ref().map(|c| c.range))
        .chain(r.finally)
}
pub(super) fn validate_regions(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    b: &mut Budget,
) -> Result<(), Error> {
    for (index, r) in f.regions.iter().enumerate() {
        check(
            r.catch.is_some() || r.finally.is_some(),
            ErrorKind::Region,
            "empty exception region",
        )?;
        for range in ranges(r) {
            check(
                range.start < range.end
                    && range.end.index() <= f.code.end_pc()
                    && f.code.is_boundary(range.start.index())
                    && f.code.is_boundary(range.end.index()),
                ErrorKind::Region,
                "exception range bounds",
            )?;
        }
        if let Some(c) = &r.catch {
            reg(f, c.exception)?;
        }
        check(
            !r.catch.as_ref().is_some_and(|c| c.range.contains(Pc(0)))
                && !r.finally.is_some_and(|range| range.contains(Pc(0))),
            ErrorKind::Region,
            "function entry inside handler",
        )?;
        let own: [Option<PcRange>; 3] = [
            Some(r.protected),
            r.catch.as_ref().map(|c| c.range),
            r.finally,
        ];
        for i in 0..3 {
            for j in i + 1..3 {
                if let (Some(a), Some(c)) = (own[i], own[j]) {
                    check(
                        a.end <= c.start || c.end <= a.start,
                        ErrorKind::Region,
                        "protected/catch/finally overlap",
                    )?;
                }
            }
        }
        if let Some(cleanup) = r.finally {
            let end = f
                .code
                .instruction(
                    f.code
                        .previous_pc(cleanup.end.index())
                        .expect("cleanup boundary"),
                )?
                .expect("verified PC");
            check(
                end.opcode == Opcode::EndFinally as u16 && end.operands[0] as usize == index,
                ErrorKind::Region,
                "finally must end with matching EndFinally",
            )?;
        }
        for other in &f.regions[..index] {
            for a in ranges(r) {
                for c in ranges(other) {
                    b.work(1)?;
                    check(
                        a != c,
                        ErrorKind::Region,
                        "ambiguous shared exception block",
                    )?;
                    check(
                        a.end <= c.start
                            || c.end <= a.start
                            || (a.start <= c.start && c.end <= a.end)
                            || (c.start <= a.start && a.end <= c.end),
                        ErrorKind::Region,
                        "partially overlapping exception blocks",
                    )?;
                }
            }
            for (inner, outer) in [(r, other), (other, r)] {
                for block in ranges(outer) {
                    b.work(3)?;
                    if ranges(inner).any(|part| block.start <= part.start && part.end <= block.end)
                    {
                        for part in ranges(inner) {
                            b.work(1)?;
                            check(
                                block.start <= part.start && part.end <= block.end,
                                ErrorKind::Region,
                                "region protected/catch/finally must share one lexical parent block",
                            )?;
                        }
                    }
                }
            }
            check(
                r.protected != other.protected,
                ErrorKind::Region,
                "duplicate protected range",
            )?;
        }
    }
    Ok(())
}
pub(super) fn validate_instruction(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    range: std::ops::Range<usize>,
    i: &Instruction,
    b: &mut Budget,
) -> Result<(), Error> {
    let pc = range.start;
    b.work(1)?;
    let spec = instruction_spec(i.opcode)
        .ok_or_else(|| Error::new(ErrorKind::Opcode, "unknown opcode"))?;
    check(
        i.flags & !spec.allowed_flags == 0,
        ErrorKind::Flags,
        "undeclared instruction flags",
    )?;
    for (kind, value) in spec.operands.iter().zip(i.operands) {
        use OperandKind::*;
        match kind {
            Unused => check(
                value == 0,
                ErrorKind::Encoding,
                "unused operand must be zero",
            )?,
            ReadReg | WriteReg | ReadWriteReg => reg(f, Reg(value))?,
            Const => {
                let constant = get(&p.constants, value, "instruction constant")?;
                if spec.opcode == Opcode::EqNumber {
                    check(
                        constant.numeric_value().is_some(),
                        ErrorKind::Encoding,
                        "EqNumber requires a numeric constant",
                    )?;
                }
            }
            Symbol => {
                symbol(p, SymbolId(value))?;
            }
            Type => ty(p, TypeId(value))?,
            Binding | BindingRead | BindingWrite | BindingInit => {
                get(&f.bindings, value, "instruction binding")?;
            }
            Capture => {
                get(&f.captures, value, "instruction capture")?;
            }
            Pc => check(
                (value as usize) < f.code.end_pc() && f.code.is_boundary(value as usize),
                ErrorKind::Index,
                "jump PC",
            )?,
            Region => {
                get(&f.regions, value, "instruction region")?;
            }
            Descriptor(kind) => check(
                get(&f.descriptors, value, "instruction descriptor")?.kind() == *kind,
                ErrorKind::Index,
                "descriptor kind mismatch",
            )?,
        }
    }
    spec.visit_registers_budgeted(f, i, |units| b.work(units), |_, _| {})?;
    if spec.may_throw {
        b.work(f.regions.len() as u64)?;
        if let Some(target) = exception_target(f, Pc(pc as u32), None) {
            let r = &f.regions[target.region.index()];
            let range = match target.kind {
                HandlerKind::Catch => r.catch.as_ref().expect("catch target").range,
                HandlerKind::Finally => r.finally.expect("finally target"),
            };
            check(
                !range.contains(Pc(pc as u32)) && range.start == target.pc,
                ErrorKind::ControlFlow,
                "exception edge re-enters its own handler",
            )?;
        }
    }
    if spec.opcode == Opcode::CellNew {
        let binding = &f.bindings[i.operands[0] as usize];
        check(
            matches!(binding.storage, BindingStorage::Cell(_))
                && binding.initialization == BindingInitialization::Explicit,
            ErrorKind::Capture,
            "CellNew requires an explicit cell binding contract",
        )?;
    }
    b.work(f.regions.len() as u64)?;
    let in_finally = f
        .regions
        .iter()
        .any(|r| r.finally.is_some_and(|range| range.contains(Pc(pc as u32))));
    let restricted =
        f.context != Context::Ordinary || in_finally || f.kind == FunctionKind::Generator;
    if spec.opcode == Opcode::Await {
        check(
            !restricted && p.signatures[f.signature.index()].effects.0 & Effects::WAIT != 0,
            ErrorKind::ControlFlow,
            "Await in restricted context/effect",
        )?;
    }
    if spec.opcode == Opcode::Yield {
        check(
            f.kind == FunctionKind::Generator
                && matches!(f.context, Context::Ordinary | Context::NoWait)
                && !in_finally,
            ErrorKind::ControlFlow,
            "Yield outside generator or in restricted context",
        )?;
    }
    if spec.opcode == Opcode::ThreadSuspend {
        check(
            f.kind == FunctionKind::Plain && !restricted,
            ErrorKind::ControlFlow,
            "ThreadSuspend in restricted context",
        )?;
    }
    if spec.opcode == Opcode::Return {
        check(!in_finally, ErrorKind::ControlFlow, "Return from finally")?;
    }
    if spec.opcode == Opcode::Rethrow {
        b.work(f.regions.len() as u64)?;
        check(
            f.regions.iter().any(|r| {
                r.catch
                    .as_ref()
                    .is_some_and(|c| c.range.contains(Pc(pc as u32)))
            }),
            ErrorKind::ControlFlow,
            "Rethrow outside catch",
        )?;
    }
    if spec.opcode == Opcode::EndFinally {
        let r = &f.regions[i.operands[0] as usize];
        check(
            r.finally
                .is_some_and(|cleanup| cleanup.end.index() == range.end),
            ErrorKind::Region,
            "EndFinally outside matching cleanup terminator",
        )?;
    }
    match spec.flow {
        Flow::Next => {
            check(
                range.end < f.code.end_pc(),
                ErrorKind::ControlFlow,
                "fallthrough past code",
            )?;
            edge(f, pc, range.end, false, b)?;
        }
        Flow::Jump => edge(f, pc, i.operands[0] as usize, false, b)?,
        Flow::Leave => {
            edge(f, pc, i.operands[0] as usize, true, b)?;
        }
        Flow::Branch => {
            edge(f, pc, i.operands[1] as usize, false, b)?;
            edge(f, pc, i.operands[2] as usize, false, b)?;
        }
        Flow::Return | Flow::Throw | Flow::EndFinally => {}
    }
    Ok(())
}
pub(super) fn edge(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    from: usize,
    to: usize,
    leave: bool,
    b: &mut Budget,
) -> Result<(), Error> {
    let from = Pc(from as u32);
    let to = Pc(to as u32);
    let mut exits = false;
    for r in &f.regions {
        b.work(1)?;
        for handler in r
            .catch
            .as_ref()
            .map(|c| c.range)
            .into_iter()
            .chain(r.finally)
        {
            if handler.contains(to) {
                check(
                    handler.contains(from),
                    ErrorKind::ControlFlow,
                    "jump into exception handler",
                )?;
            }
            if handler.contains(from) && !handler.contains(to) {
                check(
                    !r.finally.is_some_and(|range| range == handler) && leave,
                    ErrorKind::ControlFlow,
                    "handler escape requires Leave",
                )?;
                exits = true;
            }
        }
        if r.protected.contains(from) && !r.protected.contains(to) {
            check(
                leave,
                ErrorKind::ControlFlow,
                "protected exit requires Leave",
            )?;
            exits = true;
        }
        if !r.protected.contains(from) && r.protected.contains(to) {
            check(
                to == r.protected.start,
                ErrorKind::ControlFlow,
                "jump into protected interior",
            )?;
        }
    }
    if leave {
        check(
            exits,
            ErrorKind::ControlFlow,
            "Leave must exit a protected/catch region",
        )?;
    }
    Ok(())
}
pub(super) fn exception_target(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    pc: Pc,
    skip: Option<RegionId>,
) -> Option<HandlerTarget> {
    unwind_target(f, pc, skip, None)
}
pub(super) fn strict_region_ancestor(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    parent: RegionId,
    child: RegionId,
) -> bool {
    parent != child
        && ranges(&f.regions[parent.index()]).any(|block| {
            ranges(&f.regions[child.index()])
                .all(|part| block.start <= part.start && part.end <= block.end)
        })
}
pub(super) fn protected_or_catch_block(r: &ExceptionRegion, pc: Pc) -> Option<PcRange> {
    if r.protected.contains(pc) {
        Some(r.protected)
    } else {
        r.catch
            .as_ref()
            .filter(|c| c.range.contains(pc))
            .map(|c| c.range)
    }
}
pub(super) fn unwind_target(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    pc: Pc,
    skip: Option<RegionId>,
    destination: Option<CleanupDestination>,
) -> Option<HandlerTarget> {
    let mut best: Option<(u32, HandlerTarget)> = None;
    for (id, r) in f.regions.iter().enumerate() {
        let region = RegionId(id as u32);
        if let Some(consumed) = skip
            && !strict_region_ancestor(f, region, consumed)
        {
            continue;
        }
        // A finally range is intentionally not a candidate for its own region.
        let Some(range) = protected_or_catch_block(r, pc) else {
            continue;
        };
        if let Some(CleanupDestination::Leave(target)) = destination
            && range.contains(target)
        {
            continue;
        }
        let catch = (destination.is_none() && range == r.protected)
            .then_some(r.catch.as_ref())
            .flatten();
        let target = if let Some(c) = catch {
            Some(HandlerTarget {
                region,
                kind: HandlerKind::Catch,
                pc: c.range.start,
                exception: Some(c.exception),
            })
        } else {
            r.finally.map(|range| HandlerTarget {
                region,
                kind: HandlerKind::Finally,
                pc: range.start,
                exception: None,
            })
        };
        if let Some(target) = target {
            let width = range.end.0 - range.start.0;
            if best.is_none_or(|(old, _)| width < old) {
                best = Some((width, target));
            }
        }
    }
    best.map(|(_, target)| target)
}

//! Prove parameterless expression frames once at admission.
//! Eligible bodies still execute their original instructions and source PCs.
use super::*;
impl VerifiedProgram {
    pub(crate) fn compact_derived(&self, function: FunctionId) -> bool {
        self.0
            .compact_derived
            .get(function.index() / 32)
            .is_some_and(|word| word & (1 << (function.0 % 32)) != 0)
    }
    #[cfg(test)]
    pub(crate) fn without_compact_derived_plans(mut self) -> Self {
        std::sync::Arc::get_mut(&mut self.0)
            .unwrap()
            .compact_derived
            .fill(0);
        self
    }
    #[cfg(test)]
    pub(crate) fn compact_derived_capacity_bytes(&self) -> usize {
        self.0.compact_derived.capacity() * size_of::<u32>()
    }
}
pub(super) fn build(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    b: &mut Budget,
) -> Result<Vec<u32>, Error> {
    #[cfg(feature = "vm-validation-oracle")]
    let tuple_derives = std::env::var("ASIDE_VM_LEGACY_TUPLE_DERIVES").as_deref() != Ok("1");
    #[cfg(not(feature = "vm-validation-oracle"))]
    let tuple_derives = true;
    let mut eligible = b.vec::<u32>(p.functions.len().div_ceil(32))?;
    b.work(p.functions.len().div_ceil(32) as u64)?;
    eligible.resize(p.functions.len().div_ceil(32), 0);
    for (index, f) in p.functions.iter().enumerate() {
        b.work(1)?;
        let sig = &p.signatures[f.signature.index()];
        if f.kind != FunctionKind::Plain
            || f.context == Context::Init
            || !sig.parameters.is_empty()
            || sig.rest.is_some()
            || !f.bindings.is_empty()
            || !f.regions.is_empty()
            || f.code.is_empty()
        {
            continue;
        }
        b.work(f.code.len() as u64 * 3)?;
        let mut valid = true;
        for (i, instruction) in f.code.iter().enumerate() {
            let instruction = instruction?;
            let op = instruction_spec(instruction.opcode)
                .expect("verified opcode")
                .opcode;
            valid &= if i + 1 == f.code.len() {
                op == Opcode::Return
            } else {
                matches!(
                    op,
                    Opcode::Return
                        | Opcode::Jump
                        | Opcode::BranchBool
                        | Opcode::BranchNull
                        | Opcode::Call
                        | Opcode::CallNoWait
                        | Opcode::LoadConst
                        | Opcode::Move
                        | Opcode::Clear
                        | Opcode::Clear2
                        | Opcode::Clear3
                        | Opcode::CaptureRead
                        | Opcode::CellRead
                        | Opcode::GlobalRead
                        | Opcode::MemberGet
                        | Opcode::IndexGet
                        | Opcode::Add
                        | Opcode::Sub
                        | Opcode::Mul
                        | Opcode::Div
                        | Opcode::Rem
                        | Opcode::Neg
                        | Opcode::Pos
                        | Opcode::Not
                        | Opcode::BitNot
                        | Opcode::BitAnd
                        | Opcode::BitOr
                        | Opcode::BitXor
                        | Opcode::Shl
                        | Opcode::Shr
                        | Opcode::Eq
                        | Opcode::Ne
                        | Opcode::Lt
                        | Opcode::Le
                        | Opcode::Gt
                        | Opcode::Ge
                        | Opcode::Is
                        | Opcode::CheckType
                        | Opcode::TextNew
                        | Opcode::TextPush
                        | Opcode::TextFinish
                ) || op == Opcode::MakeTuple && tuple_derives
            };
        }
        if valid {
            eligible[index / 32] |= 1 << (index % 32);
        }
    }
    Ok(eligible)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_proof_accepts_expression_control_flow_but_retains_binding_restrictions() {
        let mut package = fixtures::helper_native();
        assert!(
            verify(package.clone(), Limits::default())
                .unwrap()
                .compact_derived(FunctionId(0))
        );
        assert!(
            !verify(package.clone(), Limits::default())
                .unwrap()
                .compact_derived(FunctionId(1))
        );
        let function = &mut package.functions[0];
        function.descriptors.clear();
        function.source_map.clear();
        function.code = vec![
            Instruction::new(Opcode::LoadConst, 0, 1, 0),
            Instruction::new(Opcode::Return, 0, 0, 0),
        ];
        assert!(
            verify(package.clone(), Limits::default())
                .unwrap()
                .compact_derived(FunctionId(0))
        );
        package.functions[0]
            .code
            .insert(0, Instruction::new(Opcode::Jump, 1, 0, 0));
        assert!(
            verify(package, Limits::default())
                .unwrap()
                .compact_derived(FunctionId(0))
        );
    }
}

#[test]
#[ignore = "inspect compact-expression coverage in an external B01 artifact"]
fn compact_external_program_coverage() {
    let path = std::env::var("ASIDE_B01_PATH").expect("ASIDE_B01_PATH");
    let bytes = std::fs::read(path).unwrap();
    let program = VerifiedProgram::decode(&bytes, Limits::default()).unwrap();
    if let Ok(path) = std::env::var("ASIDE_DISASSEMBLY_OUT") {
        let eligible = (0..program.package().functions.len())
            .filter(|&id| program.compact_derived(FunctionId(id as u32)))
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut text = format!("compact-functions\t{eligible}\n");
        crate::bytecode::disassemble(&program, &mut text).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut admitted = 0;
    let mut called = 0;
    let mut branched = 0;
    for (index, function) in program.package().functions.iter().enumerate() {
        if !program.compact_derived(FunctionId(index as u32)) {
            continue;
        }
        admitted += 1;
        let mut has_call = false;
        let mut has_branch = false;
        for instruction in function.code.iter() {
            let op = instruction.unwrap().opcode;
            has_call |= op == Opcode::Call as u16 || op == Opcode::CallNoWait as u16;
            has_branch |= op == Opcode::Jump as u16
                || op == Opcode::BranchBool as u16
                || op == Opcode::BranchNull as u16;
        }
        called += usize::from(has_call);
        branched += usize::from(has_branch);
    }
    eprintln!(
        "compact_coverage functions={} admitted={admitted} with_calls={called} with_branches={branched}",
        program.package().functions.len()
    );
}

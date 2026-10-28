//! Prove straight-line, parameterless expression frames once at admission.
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
            let op = instruction_spec(instruction?.opcode)
                .expect("verified opcode")
                .opcode;
            valid &= if i + 1 == f.code.len() {
                op == Opcode::Return
            } else {
                matches!(
                    op,
                    Opcode::LoadConst
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
                )
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
    fn compact_proof_keeps_dynamic_calls_and_control_flow_on_the_shared_bridge() {
        let mut package = fixtures::helper_native();
        assert!(
            !verify(package.clone(), Limits::default())
                .unwrap()
                .compact_derived(FunctionId(0))
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
            !verify(package, Limits::default())
                .unwrap()
                .compact_derived(FunctionId(0))
        );
    }
}

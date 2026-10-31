use super::*;

fn setup() -> (
    bc::FunctionDef,
    Function,
    super::super::super::budget::Budget,
) {
    let span = crate::TextRange::empty(0.into());
    let definition = bc::FunctionDef {
        module: bc::ModuleId(0),
        name: bc::SymbolId(0),
        kind: bc::FunctionKind::Plain,
        signature: bc::SignatureId(0),
        context: bc::Context::Ordinary,
        captures: vec![],
        register_count: 5,
        bindings: vec![bc::BindingDef {
            name: bc::SymbolId(0),
            ty: bc::TypeId(0),
            mutable: false,
            storage: bc::BindingStorage::Value(bc::Reg(0)),
            initialization: bc::BindingInitialization::Parameter(0),
        }],
        descriptors: vec![],
        code: vec![],
        regions: vec![],
        source_map: vec![],
    };
    let operations = [
        bc::Instruction::new(bc::Opcode::BindRead, 2, 0, 0),
        bc::Instruction::new(bc::Opcode::MemberGet, 3, 2, 0),
        bc::Instruction::new(bc::Opcode::Clear, 2, 0, 0),
    ]
    .into_iter()
    .map(|instruction| Operation { instruction, span })
    .collect();
    let function = Function {
        output: bc::FunctionId(0),
        origin: Origin::Module,
        span,
        blocks: vec![Block {
            operations,
            terminator: Some((Terminator::Return(bc::Reg(3)), span)),
        }],
        layout: vec![BlockId(0)],
        regions: vec![],
    };
    (
        definition,
        function,
        super::super::super::budget::Budget::new(Default::default()),
    )
}

#[test]
fn immutable_parameter_reads_reuse_the_original_root_and_consumer_span() {
    let (definition, mut function, mut budget) = setup();
    let span = crate::TextRange::new(10.into(), 17.into());
    function.blocks[0].operations[1].span = span;
    eliminate(&mut function, &definition, &mut budget).unwrap();
    let operations = &function.blocks[0].operations;
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0].instruction,
        bc::Instruction::new(bc::Opcode::MemberGet, 3, 0, 0)
    );
    assert_eq!(operations[0].span, span);
}

#[test]
fn unknown_old_temporary_roots_are_cleared_before_the_original_consumer() {
    let (definition, mut function, mut budget) = setup();
    let span = function.span;
    let body = function.blocks.remove(0);
    function.blocks = vec![
        Block {
            operations: vec![],
            terminator: Some((Terminator::Jump(BlockId(1)), span)),
        },
        body,
    ];
    function.layout.push(BlockId(1));
    eliminate(&mut function, &definition, &mut budget).unwrap();
    let ops = &function.blocks[1].operations;
    assert_eq!(ops.len(), 2);
    assert_eq!(
        ops[0].instruction,
        bc::Instruction::new(bc::Opcode::Clear, 2, 0, 0)
    );
    assert_eq!(
        ops[1].instruction,
        bc::Instruction::new(bc::Opcode::MemberGet, 3, 0, 0)
    );
}

#[test]
fn mutable_cell_uninitialized_and_source_overwriting_consumers_keep_the_binding_read() {
    for case in 0..4 {
        let (mut definition, mut function, mut budget) = setup();
        match case {
            0 => definition.bindings[0].mutable = true,
            1 => definition.bindings[0].storage = bc::BindingStorage::Cell(bc::Reg(0)),
            2 => definition.bindings[0].initialization = bc::BindingInitialization::Explicit,
            3 => {
                function.blocks[0].operations[1].instruction =
                    bc::Instruction::new(bc::Opcode::Move, 0, 2, 0)
            }
            _ => unreachable!(),
        }
        eliminate(&mut function, &definition, &mut budget).unwrap();
        assert_eq!(function.blocks[0].operations.len(), 3, "case {case}");
        assert_eq!(
            function.blocks[0].operations[0].instruction.opcode,
            bc::Opcode::BindRead as u16
        );
    }
}

#[test]
fn descriptor_argument_windows_keep_their_original_capture() {
    let (mut definition, mut function, mut budget) = setup();
    definition
        .descriptors
        .push(bc::Descriptor::Call(bc::CallDesc {
            target: bc::CallTarget::Function(bc::FunctionId(0)),
            contract: None,
            arguments: bc::Arguments::Window(bc::RegWindow {
                start: bc::Reg(2),
                len: 1,
            }),
            names: vec![],
        }));
    function.blocks[0].operations[1].instruction = bc::Instruction::new(bc::Opcode::Call, 3, 0, 0);
    eliminate(&mut function, &definition, &mut budget).unwrap();
    assert_eq!(function.blocks[0].operations.len(), 3);
}

#[test]
fn lowered_immutable_parameter_roots_remain_initialized_across_all_blocks() {
    let source = r#"
        fn branches(value, flag: bool) {
            if flag { value.field; } else { value.field; }
            while flag { let value = {field: 1}; value.field; break; }
            try { throw value; } catch value { value.field; }
            finally { let local = value; }
            fn capture() { value }
            capture()
        }
        fn defaults(value, other = value.field) { value.field }
        gen fn generator(value) { yield value.field; yield value.field; }
        class Reader { let field: int = 1; fn sample(self) { self.field } }
    "#;
    let module = crate::analyze_module(
        source,
        &crate::AnalysisOptions::default(),
        &mut crate::semantic::NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let ir = crate::lowering::lower(&module, &Default::default(), Default::default()).unwrap();
    let mut parameters = 0;
    for function in ir.functions() {
        let definition = &ir.tables().functions[function.output.index()];
        for binding in &definition.bindings {
            let bc::BindingStorage::Value(register) = binding.storage else {
                continue;
            };
            if binding.mutable
                || !matches!(
                    binding.initialization,
                    bc::BindingInitialization::Parameter(_)
                )
            {
                continue;
            }
            parameters += 1;
            for block in &function.blocks {
                for operation in &block.operations {
                    bc::instruction_spec(operation.instruction.opcode)
                        .unwrap()
                        .visit_registers(definition, &operation.instruction, |operand, access| {
                            assert!(
                                operand != register || access == bc::Access::Read,
                                "immutable parameter root overwritten by {:?}",
                                operation.instruction
                            );
                        })
                        .unwrap();
                }
            }
        }
    }
    assert!(
        parameters >= 5,
        "exercise ordinary, default, captured, method and generator parameters"
    );
}

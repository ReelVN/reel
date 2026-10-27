//! Default parameters and cell layout verification.
use super::*;

#[test]
fn rest_name_cannot_be_supplied_through_a_property_bag_tail() {
    let mut p = fixtures::helper_native();
    let rest_name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("rest".into());
    let property_name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("extra".into());
    p.signatures[0].rest = Some(Parameter {
        name: rest_name,
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p.signatures[0].property_bag_tail = Some(PropertyBagTail {
        target_parameter: 0,
        capability: None,
        require_writable: true,
        require_animatable: false,
    });
    let f = &mut p.functions[0];
    f.register_count = 3;
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::LoadConst, 1, 1, 0),
        Instruction::new(Opcode::Call, 2, 0, 0),
        Instruction::new(Opcode::Return, 2, 0, 0),
    ];
    f.descriptors = vec![Descriptor::Call(CallDesc {
        contract: None,
        target: CallTarget::Native(NativeId(0)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 2,
        }),
        names: vec![NamedArg {
            name: property_name,
            offset: 1,
        }],
    })];
    let v = verify(p.clone(), limits()).unwrap();
    let bytes = encode(v.package(), limits()).unwrap();
    assert_eq!(
        &VerifiedProgram::decode(&bytes, limits())
            .unwrap()
            .to_unpacked()
            .unwrap(),
        &v.to_unpacked().unwrap()
    );
    let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
        unreachable!()
    };
    call.names[0].name = rest_name;
    let e = verify(p, limits()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Signature);
    assert!(e.detail.contains("cannot supply rest"));
}
#[test]
fn windows_descriptors_and_call_signature_conflicts_fail() {
    for w in [
        RegWindow {
            start: Reg(u32::MAX),
            len: 2,
        },
        RegWindow {
            start: Reg(1),
            len: 2,
        },
    ] {
        let mut p = fixtures::helper_native();
        if let Descriptor::Call(c) = &mut p.functions[0].descriptors[0] {
            c.arguments = Arguments::Window(w);
        }
        bad(p, ErrorKind::Window);
    }
    let mut p = fixtures::helper_native();
    p.functions[0].code[1].operands[1] = 99;
    bad(p, ErrorKind::Index);
    let mut p = fixtures::helper_native();
    p.functions[0].descriptors[0] = Descriptor::Sequence(RegWindow {
        start: Reg(0),
        len: 1,
    });
    bad(p, ErrorKind::Index);
    let mut p = fixtures::helper_native();
    if let Descriptor::Call(c) = &mut p.functions[0].descriptors[0] {
        c.arguments = Arguments::Window(RegWindow {
            start: Reg(0),
            len: 0,
        });
    }
    bad(p, ErrorKind::Signature);
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].mutable = true;
    bad(p, ErrorKind::Signature);
    let mut p = fixtures::helper_native();
    p.functions[0].code[0] = Instruction::new(Opcode::Jump, 99, 0, 0);
    bad(p, ErrorKind::Index);
}
#[test]
fn cell_and_default_capture_contracts_cannot_be_forged() {
    let mut p = fixtures::cell_closures();
    p.functions[1].captures[0] = CaptureKind::Value;
    bad(p, ErrorKind::Capture);
    let mut p = fixtures::cell_closures();
    p.functions[0].bindings[0].storage = BindingStorage::Value(Reg(1));
    bad(p, ErrorKind::Capture);
    let mut p = fixtures::captured_default();
    p.functions[2].captures.clear();
    bad(p, ErrorKind::Capture);
    let mut p = fixtures::captured_default();
    p.functions[2].context = Context::Ordinary;
    bad(p, ErrorKind::Signature);
    let mut p = fixtures::captured_default();
    p.signatures[2].parameters.push(Parameter {
        name: SymbolId(4),
        ty: TypeId(1),
        mutable: false,
        default: None,
    });
    bad(p, ErrorKind::Signature);
    let mut p = fixtures::cell_closures();
    p.functions[0].code[1].operands[0] = 99;
    bad(p, ErrorKind::Index);
    let mut p = fixtures::helper_native();
    p.functions[1].bindings[0].initialization = BindingInitialization::Parameter(99);
    bad(p, ErrorKind::Signature);
}
#[test]
fn forward_readonly_cell_carries_initialization_and_write_contract() {
    let p = verify(fixtures::forward_readonly_cell(), limits()).unwrap();
    let f = &p.package().functions[0];
    assert_eq!(
        f.bindings[0].initialization,
        BindingInitialization::Explicit
    );
    assert!(!f.bindings[0].mutable);
    assert_eq!(
        f.code.instruction(0).unwrap().unwrap(),
        Instruction::new(Opcode::CellNew, 0, 0, 0)
    );
    assert_eq!(
        f.code.iter().map(Result::unwrap).nth(1).unwrap().opcode,
        Opcode::MakeClosure as u16
    );
    assert_eq!(
        f.code.iter().map(Result::unwrap).nth(3).unwrap().opcode,
        Opcode::BindInit as u16
    );
    assert_eq!(
        Opcode::CellNew.spec().operands,
        [
            OperandKind::Binding,
            OperandKind::Unused,
            OperandKind::Unused
        ]
    );
    assert_eq!(
        Opcode::CellRead.spec().semantic_rule,
        SemanticRule::BindingRead
    );
    assert_eq!(
        Opcode::CellWrite.spec().semantic_rule,
        SemanticRule::BindingWrite
    );
    let mut register_access = Vec::new();
    Opcode::CellNew
        .spec()
        .visit_registers(f, &f.code.instruction(0).unwrap().unwrap(), |r, a| {
            register_access.push((r, a))
        })
        .unwrap();
    assert_eq!(
        register_access,
        vec![(Reg(1), Access::Write)],
        "allocating an uninitialized cell reads no value slot"
    );
}
#[test]
fn modules_structural_types_rest_schema_and_lexical_initializers_roundtrip() {
    let mut p = gallery(Opcode::MakeClass);
    let module_path = SymbolId(p.symbols.len() as u32);
    p.symbols.push("game::imports".into());
    p.modules[0].globals[0].mutable = false;
    p.modules[0].globals[0].linked_function = Some(FunctionId(1));
    p.modules[0].exports.push(Export {
        name: SymbolId(9),
        global: GlobalId(0),
    });
    p.modules.push(ModuleDef {
        path: module_path,
        imports: vec![Import {
            name: SymbolId(9),
            module: ModuleId(0),
            export: SymbolId(9),
            content_version: p.content_version,
        }],
        exports: vec![],
        globals: vec![],
        initializer: Some(FunctionId(3)),
    });
    p.types.extend([
        TypeDef::Tuple(vec![TypeId(1)]),
        TypeDef::Union(vec![TypeId(0), TypeId(1)]),
        TypeDef::Function {
            parameters: vec![TypeId(1)],
            return_type: TypeId(0),
        },
        TypeDef::Named {
            module: ModuleId(1),
            name: SymbolId(12),
            version: p.content_version,
        },
    ]);
    p.signatures[0].rest = Some(Parameter {
        name: SymbolId(13),
        ty: TypeId(0),
        mutable: true,
        default: None,
    });
    p.signatures[0].property_bag_tail = Some(PropertyBagTail {
        target_parameter: 0,
        capability: Some(SymbolId(5)),
        require_writable: true,
        require_animatable: true,
    });
    let mut init_signature = p.signatures[1].clone();
    init_signature.effects = Effects(Effects::ALLOCATE);
    p.signatures.push(init_signature.clone());
    p.signatures.push(init_signature);
    let mut lexical = p.functions[1].clone();
    lexical.signature = SignatureId(2);
    lexical.context = Context::NoWait;
    lexical.captures = vec![CaptureKind::Value];
    lexical.bindings.clear();
    lexical.descriptors.clear();
    lexical.source_map.clear();
    lexical.code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    p.functions.push(lexical);
    let mut module_init = p.functions[2].clone();
    module_init.module = ModuleId(1);
    module_init.signature = SignatureId(3);
    module_init.context = Context::Init;
    module_init.captures.clear();
    module_init.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    p.functions.push(module_init);
    if let Descriptor::Class(class) = &mut p.functions[0].descriptors[5] {
        class.fields[0].initializer = Some(FunctionId(2));
        class.fields[0].captures = vec![CaptureSource::Capture(CaptureId(0))];
        class.methods[0].function = FunctionId(2);
        class.methods[0].captures = vec![CaptureSource::Capture(CaptureId(0))];
    }
    if let Descriptor::Pattern(pattern) = &mut p.functions[0].descriptors[8]
        && let PatternNode::Object { fields, .. } = &mut pattern.nodes[5]
    {
        fields[0].default = Some(FunctionId(2));
        fields[0].captures = vec![CaptureSource::Capture(CaptureId(0))];
    }
    let bytes = encode(&p, limits()).unwrap();
    assert_eq!(
        &VerifiedProgram::decode(&bytes, limits())
            .unwrap()
            .to_unpacked()
            .unwrap(),
        &p
    );
    let mut wrong = p.clone();
    wrong.modules[1].imports[0].content_version = 2;
    bad(wrong, ErrorKind::Version);
    let mut wrong = p.clone();
    wrong.signatures[0]
        .property_bag_tail
        .as_mut()
        .unwrap()
        .target_parameter = 99;
    bad(wrong, ErrorKind::Signature);
    p.types.push(TypeDef::Function {
        parameters: vec![],
        return_type: TypeId(p.types.len() as u32),
    });
    bad(p, ErrorKind::Index);
}

#[test]
fn shared_defaults_cannot_hide_function_parameter_capture_product_work() {
    const FUNCTIONS: u32 = 100;
    const PARAMETERS: u32 = 100;
    const CAPTURES: usize = 1000;
    let mut p = budget_package(PARAMETERS, 0);
    let inputs = p.signatures[0].parameters.clone();
    for i in 0..PARAMETERS {
        let mut signature = p.signatures[0].clone();
        signature.parameters = inputs[..i as usize].to_vec();
        p.signatures.push(signature);
        p.signatures[0].parameters[i as usize].default =
            Some(ParameterDefault::Function(FunctionId(FUNCTIONS + i)));
    }
    p.functions = (0..FUNCTIONS)
        .map(|_| budget_function(0, PARAMETERS, CAPTURES))
        .chain((0..PARAMETERS).map(|i| {
            let mut f = budget_function(i + 1, i, CAPTURES);
            f.context = Context::NoWait;
            f
        }))
        .collect();
    let stats = verify(p.clone(), limits()).unwrap().stats();
    assert!(stats.records < limits().max_records as u64);
    assert!(stats.encoded_bytes < 512 * 1024);
    let comparisons = FUNCTIONS as u64 * PARAMETERS as u64 * CAPTURES as u64;
    assert!(stats.validation_work >= comparisons + stats.encoded_bytes as u64);
    let bytes = super::super::codec::encode_unverified(&p);
    let small = Limits {
        max_work: 1_000_000,
        ..limits()
    };
    decode(&bytes, small).unwrap();
    let began = std::time::Instant::now();
    let error = VerifiedProgram::decode(&bytes, small).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Limit);
    assert!(error.function.is_some_and(|f| f.0 < FUNCTIONS));
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    println!(
        "shared-default-product: bytes={} records={} full_work={} limit={} rejected_function={:?} elapsed={:?}",
        bytes.len(),
        stats.records,
        stats.validation_work,
        small.max_work,
        error.function,
        began.elapsed()
    );
}

#[test]
fn every_function_prepays_its_shared_parameter_scan_without_defaults() {
    const FUNCTIONS: u32 = 100;
    const PARAMETERS: u32 = 100;
    let mut p = budget_package(PARAMETERS, 0);
    let mut empty = p.signatures[0].clone();
    empty.parameters.clear();
    p.signatures.push(empty);
    p.functions = (0..FUNCTIONS)
        .map(|_| budget_function(1, PARAMETERS, 0))
        .collect();
    let baseline = verify(p.clone(), limits()).unwrap().stats();
    for f in &mut p.functions {
        f.signature = SignatureId(0);
    }
    let full = verify(p.clone(), limits()).unwrap().stats();
    assert_eq!(full.encoded_bytes, baseline.encoded_bytes);
    let scan_work = FUNCTIONS as u64 * PARAMETERS as u64;
    assert!(full.validation_work >= baseline.validation_work + scan_work);
    assert_shared_scan_limit(p, baseline.validation_work + scan_work / 2);
}

#[test]
fn every_native_prepays_its_shared_parameter_scan() {
    const NATIVES: u32 = 100;
    const PARAMETERS: u32 = 100;
    let mut p = budget_package(PARAMETERS, NATIVES);
    let mut empty = p.signatures[0].clone();
    empty.parameters.clear();
    p.signatures.push(empty);
    p.natives = (0..NATIVES)
        .map(|i| NativeDef {
            symbol: SymbolId(2 + PARAMETERS + i),
            signature: SignatureId(1),
            abi_version: 1,
            capabilities: vec![],
        })
        .collect();
    let baseline = verify(p.clone(), limits()).unwrap().stats();
    for native in &mut p.natives {
        native.signature = SignatureId(0);
    }
    let full = verify(p.clone(), limits()).unwrap().stats();
    assert_eq!(full.encoded_bytes, baseline.encoded_bytes);
    let scan_work = NATIVES as u64 * PARAMETERS as u64;
    let other_work = baseline.validation_work;
    assert!(full.validation_work >= other_work + scan_work);
    assert_shared_scan_limit(p, other_work + scan_work / 2);
}

#[test]
fn property_bag_capability_searches_are_charged_for_each_signature() {
    const SIGNATURES: u32 = 100;
    const CAPABILITIES: u32 = 100;
    let mut p = budget_package(1, CAPABILITIES);
    p.capabilities = (0..CAPABILITIES).map(|i| SymbolId(3 + i)).collect();
    p.signatures = vec![p.signatures[0].clone(); SIGNATURES as usize];
    let baseline = verify(p.clone(), limits()).unwrap().stats();
    for signature in &mut p.signatures {
        signature.property_bag_tail = Some(PropertyBagTail {
            target_parameter: 0,
            capability: p.capabilities.last().copied(),
            require_writable: false,
            require_animatable: false,
        });
    }
    let full = verify(p.clone(), limits()).unwrap().stats();
    let wire_work = (full.encoded_bytes - baseline.encoded_bytes) as u64;
    let scan_work = SIGNATURES as u64 * CAPABILITIES as u64;
    let other_work = baseline.validation_work + wire_work;
    assert!(full.validation_work >= other_work + scan_work);
    assert_shared_scan_limit(p, other_work + scan_work / 2);
}

#[test]
fn positional_only_call_descriptors_prepay_shared_required_parameter_scan() {
    const CALLS: u32 = 100;
    const PARAMETERS: u32 = 100;
    let mut p = budget_package(PARAMETERS, 1);
    let mut empty = p.signatures[0].clone();
    empty.parameters.clear();
    p.signatures.push(empty);
    p.natives.push(NativeDef {
        symbol: SymbolId(2 + PARAMETERS),
        signature: SignatureId(0),
        abi_version: 1,
        capabilities: vec![],
    });
    p.functions.push(budget_function(1, PARAMETERS, 0));
    let baseline = verify(p.clone(), limits()).unwrap().stats();
    p.functions[0].descriptors = vec![
        Descriptor::Call(CallDesc {
            contract: None,
            target: CallTarget::Native(NativeId(0)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(0),
                len: PARAMETERS,
            }),
            names: vec![],
        });
        CALLS as usize
    ];
    let full = verify(p.clone(), limits()).unwrap().stats();
    let wire_work = (full.encoded_bytes - baseline.encoded_bytes) as u64;
    let scan_work = CALLS as u64 * PARAMETERS as u64;
    let other_work = baseline.validation_work + wire_work;
    assert!(full.validation_work >= other_work + scan_work);
    assert_shared_scan_limit(p, other_work + scan_work / 2);
}

#[test]
fn ordinary_project_loading_scales_under_default_limits() {
    let support = RuntimeSupport {
        host_types: Vec::new(),
        opcodes: INSTRUCTION_SPECS.iter().map(|s| s.opcode).collect(),
        natives: vec![],
        capabilities: vec![],
    };
    for count in [1000, 5000, 10_000] {
        let p = ordinary_large_package(count);
        let bytes = super::super::codec::encode_unverified(&p);
        let began = std::time::Instant::now();
        let loaded = VerifiedProgram::load(&bytes, limits(), &support);
        println!(
            "ordinary-project functions={count} symbols={} bytes={} load={:?} result={:?}",
            p.symbols.len(),
            bytes.len(),
            began.elapsed(),
            loaded.as_ref().map(|v| v.stats()).map_err(|e| e.kind)
        );
        let loaded = loaded.expect("ordinary project must load with unchanged default limits");
        assert_eq!(loaded.package().functions.len(), count as usize);
        assert_eq!(encode(loaded.package(), limits()).unwrap(), bytes);
        assert!(loaded.stats().validation_work < count as u64 * 300);
        // A host registry containing repeated entries is normalized once as well.
        loaded
            .require_runtime(&RuntimeSupport {
                host_types: Vec::new(),
                opcodes: [Opcode::LoadConst, Opcode::Return].repeat(5000),
                natives: vec![],
                capabilities: vec![],
            })
            .unwrap();
    }
}

#[test]
fn constant_defaults_validate_pool_indices_and_keep_rest_default_forbidden() {
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].default = Some(ParameterDefault::Constant(ConstId(u32::MAX)));
    bad(p, ErrorKind::Index);

    let mut p = fixtures::helper_native();
    let mut rest = p.signatures[0].parameters[0].clone();
    rest.name = SymbolId(13);
    rest.default = Some(ParameterDefault::Constant(ConstId(0)));
    p.signatures[0].rest = Some(rest);
    bad(p, ErrorKind::Signature);
}

#[test]
fn parameter_cell_default_is_an_internal_prefix_and_an_ordinary_call_target() {
    let p = fixtures::parameter_cell_default();
    let bytes = encode(&p, limits()).unwrap();
    let program = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(&program.to_unpacked().unwrap(), &p);
    assert_eq!(
        p.signatures[1].parameters[1].default,
        Some(ParameterDefault::Function(FunctionId(2)))
    );
    for function in [1, 2] {
        let binding = &p.functions[function].bindings[0];
        assert_eq!(binding.initialization, BindingInitialization::Parameter(0));
        assert_eq!(binding.storage, BindingStorage::Cell(Reg(0)));
        assert!(binding.mutable);
    }
    let Descriptor::Call(call) = &p.functions[0].descriptors[1] else {
        panic!("ordinary call")
    };
    assert_eq!(call.target, CallTarget::Function(FunctionId(2)));
    assert_eq!(
        call.arguments,
        Arguments::Window(RegWindow {
            start: Reg(0),
            len: 1
        })
    );
    let mut listing = String::new();
    disassemble(&program, &mut listing).unwrap();
    assert!(listing.contains("Cell(Reg(0))") && listing.contains("Parameter(0)"));
}

#[test]
fn default_prefix_name_type_mutability_storage_and_unique_bindings_are_checked() {
    let base = fixtures::parameter_cell_default();
    let mut p = base.clone();
    p.signatures[2].parameters[0].name = SymbolId(14);
    bad(p, ErrorKind::Signature);
    let mut p = base.clone();
    p.signatures[2].parameters[0].ty = TypeId(0);
    bad(p, ErrorKind::Signature);
    let mut p = base.clone();
    p.signatures[2].parameters[0].mutable = false;
    bad(p, ErrorKind::Signature);
    for function in [1, 2] {
        for field in 0..6 {
            let mut p = base.clone();
            let binding = &mut p.functions[function].bindings[0];
            match field {
                0 => binding.name = SymbolId(14),
                1 => binding.ty = TypeId(0),
                2 => binding.mutable = false,
                3 => binding.storage = BindingStorage::Value(Reg(0)),
                4 => binding.storage = BindingStorage::Cell(Reg(1)),
                5 => {
                    let duplicate = binding.clone();
                    p.functions[function].bindings.push(duplicate);
                }
                _ => unreachable!(),
            }
            bad(p, ErrorKind::Signature);
        }
    }
    let mut p = base.clone();
    for function in [1, 2] {
        p.functions[function].bindings[0].storage = BindingStorage::Value(Reg(0));
    }
    bad(p, ErrorKind::Signature); // Matching Value copies cannot share a mutable prefix.
    let mut p = base;
    p.functions[2].code[0] = Instruction::new(Opcode::CellNew, 0, 0, 0);
    bad(p, ErrorKind::Capture); // The binder, rather than CellNew, owns Parameter initialization.
}

#[test]
fn every_owner_of_a_shared_signature_must_match_default_parameter_storage() {
    let mut p = fixtures::parameter_cell_default();
    p.functions.push(p.functions[1].clone());
    verify(p.clone(), limits()).unwrap();
    p.functions[4].bindings[0].storage = BindingStorage::Value(Reg(0));
    let bytes = super::super::codec::encode_unverified(&p);
    for error in [
        verify(p, limits()).unwrap_err(),
        VerifiedProgram::decode(&bytes, limits()).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Signature);
        assert_eq!(error.function, Some(FunctionId(4)));
    }
    let mut p = fixtures::captured_default();
    p.symbols.push("game::other".into());
    let mut module = p.modules[0].clone();
    module.path = SymbolId(p.symbols.len() as u32 - 1);
    p.modules.push(module);
    p.functions[2].module = ModuleId(1);
    bad(p, ErrorKind::Capture);
}

#[test]
fn readonly_prefix_and_rest_parameter_cells_use_existing_binding_fields() {
    for cell in [false, true] {
        let mut p = fixtures::parameter_cell_default();
        for signature in [1, 2] {
            p.signatures[signature].parameters[0].mutable = false;
        }
        for function in [1, 2] {
            let binding = &mut p.functions[function].bindings[0];
            binding.mutable = false;
            binding.storage = if cell {
                BindingStorage::Cell(Reg(0))
            } else {
                BindingStorage::Value(Reg(0))
            };
            p.functions[function].code = vec![
                Instruction::new(Opcode::BindRead, 2, 0, 0),
                Instruction::new(Opcode::Return, 2, 0, 0),
            ];
            p.functions[function].descriptors.clear();
        }
        let bytes = encode(&p, limits()).unwrap();
        assert_eq!(
            &VerifiedProgram::decode(&bytes, limits())
                .unwrap()
                .to_unpacked()
                .unwrap(),
            &p
        );
    }
    let mut p = budget_package(0, 1);
    p.signatures[0].rest = Some(Parameter {
        name: SymbolId(2),
        ty: TypeId(0),
        mutable: true,
        default: None,
    });
    let mut f = budget_function(0, 2, 0);
    f.bindings = vec![BindingDef {
        name: SymbolId(2),
        ty: TypeId(0),
        mutable: true,
        storage: BindingStorage::Cell(Reg(0)),
        initialization: BindingInitialization::Parameter(0),
    }];
    f.code = vec![
        Instruction::new(Opcode::BindRead, 1, 0, 0),
        Instruction::new(Opcode::Return, 1, 0, 0),
    ];
    p.functions.push(f);
    let bytes = encode(&p, limits()).unwrap();
    assert_eq!(
        &VerifiedProgram::decode(&bytes, limits())
            .unwrap()
            .to_unpacked()
            .unwrap(),
        &p
    );
    p.functions[0].bindings[0].initialization = BindingInitialization::Parameter(1);
    bad(p, ErrorKind::Signature);
}

#[test]
fn shared_default_prefix_layout_comparisons_are_precharged() {
    const FUNCTIONS: u32 = 100;
    const PARAMETERS: u32 = 100;
    let mut p = budget_package(PARAMETERS, 0);
    let prefix = p.signatures[0].parameters.clone();
    for i in 0..PARAMETERS {
        let mut s = p.signatures[0].clone();
        s.parameters = prefix[..i as usize].to_vec();
        p.signatures.push(s);
        p.signatures[0].parameters[i as usize].default =
            Some(ParameterDefault::Function(FunctionId(FUNCTIONS + i)));
    }
    p.functions = (0..FUNCTIONS)
        .map(|_| budget_function(0, PARAMETERS, 0))
        .chain((0..PARAMETERS).map(|i| {
            let mut f = budget_function(i + 1, i, 0);
            f.context = Context::NoWait;
            f
        }))
        .collect();
    let stats = verify(p.clone(), limits()).unwrap().stats();
    let pairs = FUNCTIONS as u64 * PARAMETERS as u64 * (PARAMETERS - 1) as u64 / 2;
    assert!(stats.validation_work >= pairs + stats.encoded_bytes as u64);
    let bytes = super::super::codec::encode_unverified(&p);
    assert!(bytes.len() < 128 * 1024);
    let small = Limits {
        max_work: 300_000,
        ..limits()
    };
    decode(&bytes, small).unwrap();
    let error = VerifiedProgram::decode(&bytes, small).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Limit);
    assert!(error.function.is_some_and(|f| f.0 < FUNCTIONS));
}

#[test]
fn parameter_layout_index_is_sparse_under_shared_large_signatures() {
    let mut p = budget_package(1000, 0);
    p.functions = (0..20).map(|_| budget_function(0, 1000, 0)).collect();
    let bytes = encode(&p, limits()).unwrap();
    let program = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(&program.to_unpacked().unwrap(), &p);
    let code_scratch = program
        .code_cache_stats()
        .unwrap()
        .construction_scratch_bytes
        + p.modules.len() * 4;
    assert!(program.stats().validation_scratch_peak <= (p.symbols.len() * 16).max(code_scratch));
}

#[test]
fn native_defaults_publish_one_canonical_sparse_layout_without_wire_fields() {
    let p = fixtures::native_parameter_defaults();
    let bytes = encode(&p, limits()).unwrap();
    let program = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(&program.to_unpacked().unwrap(), &p);
    assert_eq!(encode(program.package(), limits()).unwrap(), bytes);
    assert!(program.native_parameters_need_cells(NativeId(0)).unwrap());
    for (i, expected) in [
        BindingStorage::Cell(Reg(0)),
        BindingStorage::Cell(Reg(1)),
        BindingStorage::Value(Reg(2)),
        BindingStorage::Value(Reg(3)),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            program
                .native_parameter_storage(NativeId(0), i as u32)
                .unwrap(),
            expected
        );
    }
    assert_eq!(parameter_layout_bytes(&program), 2 * 8 + 2 * 4);
    assert_eq!(
        program
            .native_parameter_storage(NativeId(0), 4)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
    assert_eq!(
        program
            .native_parameters_need_cells(NativeId(1))
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
    assert_eq!(
        program
            .native_parameter_storage(NativeId(1), 0)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
    let ordinary = verify(fixtures::helper_native(), limits()).unwrap();
    assert!(!ordinary.native_parameters_need_cells(NativeId(0)).unwrap());
    assert_eq!(parameter_layout_bytes(&ordinary), 2 * 4);
    assert_eq!(
        ordinary.native_parameter_storage(NativeId(0), 0).unwrap(),
        BindingStorage::Value(Reg(0))
    );
    let mut no_defaults = p;
    for param in &mut no_defaults.signatures[1].parameters {
        param.default = None;
    }
    no_defaults.functions[0].descriptors.clear();
    no_defaults.functions[0].code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let ordinary = verify(no_defaults, limits()).unwrap();
    assert!(!ordinary.native_parameters_need_cells(NativeId(0)).unwrap());
    assert_eq!(parameter_layout_bytes(&ordinary), 2 * 4);
    assert_eq!(
        ordinary.native_parameter_storage(NativeId(0), 0).unwrap(),
        BindingStorage::Value(Reg(0))
    );
}

#[test]
fn native_default_prefix_contracts_and_mutable_cell_requirements_are_checked() {
    let base = fixtures::native_parameter_defaults();
    for function in [1, 2] {
        for field in 0..7 {
            let mut p = base.clone();
            match field {
                0 => p.signatures[function + 1].parameters[0].name = SymbolId(14),
                1 => p.signatures[function + 1].parameters[0].ty = TypeId(0),
                2 => p.signatures[function + 1].parameters[0].mutable = false,
                3 => p.functions[function].bindings[0].storage = BindingStorage::Value(Reg(0)),
                4 => p.functions[function].bindings[0].storage = BindingStorage::Cell(Reg(1)),
                5 => {
                    let duplicate = p.functions[function].bindings[0].clone();
                    p.functions[function].bindings.push(duplicate);
                }
                6 => p.functions[function].bindings[0].ty = TypeId(0),
                _ => unreachable!(),
            }
            bad(p, ErrorKind::Signature);
        }
    }
    let mut p = base;
    for function in [1, 2] {
        p.functions[function].bindings[0].storage = BindingStorage::Value(Reg(0));
    }
    bad(p, ErrorKind::Signature);
}

#[test]
fn native_readonly_prefix_uses_the_first_thunk_and_rejects_later_disagreement() {
    for cell in [false, true] {
        let mut p = fixtures::native_parameter_defaults();
        for signature in [1, 2, 3] {
            p.signatures[signature].parameters[0].mutable = false;
        }
        for function in [1, 2] {
            p.functions[function].bindings[0].mutable = false;
            p.functions[function].bindings[0].storage = if cell {
                BindingStorage::Cell(Reg(0))
            } else {
                BindingStorage::Value(Reg(0))
            };
            p.functions[function]
                .code
                .retain(|i| i.opcode != Opcode::BindWrite as u16);
        }
        let program = verify(p.clone(), limits()).unwrap();
        assert_eq!(
            program.native_parameter_storage(NativeId(0), 0).unwrap(),
            p.functions[1].bindings[0].storage
        );
        for function in [1, 2] {
            let mut inconsistent = p.clone();
            inconsistent.functions[function].bindings[0].storage = if cell {
                BindingStorage::Value(Reg(0))
            } else {
                BindingStorage::Cell(Reg(0))
            };
            bad(inconsistent, ErrorKind::Signature);
        }
    }
}

#[test]
fn shared_native_signatures_do_not_duplicate_retained_parameter_layouts() {
    let mut p = fixtures::native_parameter_defaults();
    let single = verify(p.clone(), limits()).unwrap().stats();
    for i in 1..100 {
        p.symbols.push(format!("host::defaults_{i}"));
        let mut native = p.natives[0].clone();
        native.symbol = SymbolId(p.symbols.len() as u32 - 1);
        p.natives.push(native);
    }
    let program = verify(p, limits()).unwrap();
    assert_eq!(
        program.stats().derived_capacity_bytes,
        single.derived_capacity_bytes
    );
    for i in 0..100 {
        assert!(program.native_parameters_need_cells(NativeId(i)).unwrap());
        assert_eq!(
            program.native_parameter_storage(NativeId(i), 1).unwrap(),
            BindingStorage::Cell(Reg(1))
        );
    }
}

use super::*;

type Fixture = (&'static str, fn() -> BytecodePackage, &'static [u8]);
const FIXTURES: &[Fixture] = &[
    (
        "helper-native",
        fixtures::helper_native,
        include_bytes!("testdata/helper-native.ab"),
    ),
    (
        "cell-closures",
        fixtures::cell_closures,
        include_bytes!("testdata/cell-closures.ab"),
    ),
    (
        "exception-regions",
        fixtures::exception_regions,
        include_bytes!("testdata/exception-regions.ab"),
    ),
    (
        "captured-default",
        fixtures::captured_default,
        include_bytes!("testdata/captured-default.ab"),
    ),
    (
        "duplicate-arguments",
        fixtures::duplicate_arguments,
        include_bytes!("testdata/duplicate-arguments.ab"),
    ),
];
fn limits() -> Limits {
    Limits::default()
}
fn bad(p: BytecodePackage, kind: ErrorKind) {
    let bytes = super::codec::encode_unverified(&p);
    assert_eq!(verify(p, limits()).unwrap_err().kind, kind);
    let decoded = VerifiedProgram::decode(&bytes, limits()).unwrap_err();
    // The scan may reject an index before the later semantic conflict is read.
    assert!(
        decoded.kind == kind || decoded.kind == ErrorKind::Index,
        "{decoded}"
    );
}
#[test]
fn shared_fixtures_roundtrip_source_and_disassembly() {
    for (name, constructor, golden) in FIXTURES {
        let p = constructor();
        let bytes = encode(&p, limits()).unwrap();
        if std::env::var_os("REEL_BYTECODE_WRITE_FIXTURES").is_some() {
            std::fs::write(
                format!(
                    "{}/src/bytecode/testdata/{name}.ab",
                    env!("CARGO_MANIFEST_DIR")
                ),
                &bytes,
            )
            .unwrap();
        } else {
            assert_eq!(&bytes, golden, "wire snapshot {name}");
        }
        let decoded = decode(&bytes, limits()).unwrap();
        assert_eq!(decoded, p);
        assert_eq!(encode(&decoded, limits()).unwrap(), bytes);
        let original = verify(p, limits()).unwrap();
        let loaded = verify(decoded, limits()).unwrap();
        let mut a = String::new();
        let mut b = String::new();
        disassemble(&original, &mut a).unwrap();
        disassemble(&loaded, &mut b).unwrap();
        // Allocator-retained capacities can differ; the semantic listing must not.
        assert_eq!(
            a.lines().skip(1).collect::<Vec<_>>(),
            b.lines().skip(1).collect::<Vec<_>>()
        );
        for (fi, f) in original.package().functions.iter().enumerate() {
            for map in &f.source_map {
                assert_eq!(
                    loaded.source_at(FunctionId(fi as u32), map.pc),
                    Some(map.span)
                );
            }
        }
        println!("{name}: {:?}", loaded.stats());
    }
}
#[test]
fn instruction_wire_is_exact_little_endian_and_not_enum_layout() {
    let i = Instruction::new(Opcode::BranchBool, 0x12345678, 0x90abcdef, 0x10203040);
    let expected = [
        0x31, 0, 0, 0, 0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x90, 0x40, 0x30, 0x20, 0x10,
    ];
    assert_eq!(i.to_le_bytes(), expected);
    assert_eq!(Instruction::from_le_bytes(expected).unwrap(), i);
    assert_eq!(std::mem::size_of::<Instruction>(), 16);
    let mut bad = expected;
    bad[0] = 0xff;
    bad[1] = 0xff;
    assert_eq!(
        Instruction::from_le_bytes(bad).unwrap_err().kind,
        ErrorKind::Opcode
    );
    bad = expected;
    bad[2] = 1;
    assert_eq!(
        Instruction::from_le_bytes(bad).unwrap_err().kind,
        ErrorKind::Flags
    );
}
#[test]
fn all_truncation_points_are_rejected_even_with_rewritten_total_length() {
    for (_, constructor, _) in FIXTURES {
        let bytes = encode(&constructor(), limits()).unwrap();
        for end in 0..bytes.len() {
            let mut truncated = bytes[..end].to_vec();
            if end >= 32 {
                truncated[28..32].copy_from_slice(&(end as u32).to_le_bytes());
            }
            assert!(
                decode(&truncated, limits()).is_err(),
                "accepted truncated input at {end}"
            );
        }
    }
}
#[test]
fn complete_numeric_and_immutable_constant_bits_roundtrip() {
    let mut p = fixtures::helper_native();
    p.constants.extend([
        Const::Int(i64::MIN),
        Const::Int(i64::MAX),
        Const::FloatBits(0x7ff8123456789abc),
        Const::FloatBits((-0.0f64).to_bits()),
        Const::Utf8("名字🎬".into()),
        Const::Bytes(vec![0, 255, 1]),
        Const::Bool(true),
        Const::Tuple(vec![ConstId(0), ConstId(3), ConstId(5)]),
    ]);
    let bytes = encode(&p, limits()).unwrap();
    assert_eq!(decode(&bytes, limits()).unwrap(), p);
    p.constants[3] = Const::Tuple(vec![ConstId(3)]);
    bad(p, ErrorKind::Index);
}
#[test]
fn declared_counts_overflow_and_bad_indices_do_not_allocate_payloads() {
    let bytes = encode(&fixtures::helper_native(), limits()).unwrap();
    for offset in [32, 36, 40, 44, 48, 52, 56, 60, 64, 68] {
        let mut corrupt = bytes.clone();
        corrupt[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let allocation = allocation_counter::measure(|| {
            assert!(decode(&corrupt, limits()).is_err());
        });
        assert!(
            allocation.bytes_total < 2048,
            "bad count allocated {}",
            allocation.bytes_total
        );
    }
    let instruction = Instruction::new(Opcode::LoadConst, 0, 1, 0).to_le_bytes();
    let position = bytes.windows(16).position(|w| w == instruction).unwrap();
    for operand in [4, 8] {
        let mut corrupt = bytes.clone();
        corrupt[position + operand..position + operand + 4]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                decode(&corrupt, limits()).unwrap_err().kind,
                ErrorKind::Index
            );
        });
        assert!(
            allocation.bytes_total < 2048,
            "bad index allocated payloads"
        );
    }
}
#[test]
fn budgets_include_retained_capacity_and_aggregate_nested_data() {
    let mut p = fixtures::helper_native();
    p.constants.reserve(8192);
    let estimated = verify(p.clone(), limits()).unwrap().stats();
    // Clone drops spare capacity; verify the original accounts for the retained pool.
    let retained = verify(p.clone(), limits())
        .unwrap()
        .stats()
        .owned_capacity_bytes;
    let mut tight = limits();
    tight.max_allocation_bytes = retained + 1024;
    assert_eq!(verify(p, tight).unwrap_err().kind, ErrorKind::Limit);
    let p = fixtures::helper_native();
    let bytes = encode(&p, limits()).unwrap();
    for tight in [
        Limits {
            max_bytes: bytes.len() - 1,
            ..limits()
        },
        Limits {
            max_records: 1,
            ..limits()
        },
        Limits {
            max_work: 1,
            ..limits()
        },
        Limits {
            max_allocation_bytes: estimated.owned_capacity_bytes / 2,
            ..limits()
        },
        Limits {
            max_registers: 1,
            ..limits()
        },
        Limits {
            max_string_bytes: 2,
            ..limits()
        },
    ] {
        assert_eq!(
            VerifiedProgram::decode(&bytes, tight).unwrap_err().kind,
            ErrorKind::Limit
        );
    }
}
#[test]
fn unknown_version_tags_flags_unused_operands_and_trailing_bytes_fail() {
    let bytes = encode(&fixtures::helper_native(), limits()).unwrap();
    for offset in [8, 12, 16] {
        let mut b = bytes.clone();
        b[offset] = 99;
        assert_eq!(decode(&b, limits()).unwrap_err().kind, ErrorKind::Version);
    }
    let mut b = bytes.clone();
    b[0] = 0;
    assert_eq!(decode(&b, limits()).unwrap_err().kind, ErrorKind::Encoding);
    let mut b = bytes.clone();
    b.push(0);
    let len = b.len() as u32;
    b[28..32].copy_from_slice(&len.to_le_bytes());
    assert_eq!(decode(&b, limits()).unwrap_err().kind, ErrorKind::Encoding);
    for (op, kind) in [
        (u16::MAX, ErrorKind::Opcode),
        (Opcode::LoadConst as u16, ErrorKind::Flags),
    ] {
        let mut p = fixtures::helper_native();
        p.functions[0].code[0].opcode = op;
        if kind == ErrorKind::Flags {
            p.functions[0].code[0].flags = 1;
        }
        bad(p, kind);
    }
    let mut p = fixtures::helper_native();
    p.functions[0].code[0].operands[2] = 1;
    bad(p, ErrorKind::Encoding);
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
    p.functions[0].code[1].operands[0] = 2;
    bad(p, ErrorKind::Capture);
    let mut p = fixtures::helper_native();
    p.functions[1].bindings[0].initialization = BindingInitialization::Parameter(99);
    bad(p, ErrorKind::Signature);
}
#[test]
fn every_normal_handler_entry_and_cleanup_bypass_is_rejected() {
    for instruction in [
        Instruction::new(Opcode::Jump, 2, 0, 0),
        Instruction::new(Opcode::BranchBool, 0, 2, 6),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::Jump, 6, 0, 0),
    ] {
        let mut p = fixtures::exception_regions();
        p.functions[0].code[1] = instruction;
        bad(p, ErrorKind::ControlFlow);
    }
    for instruction in [
        Instruction::new(Opcode::Jump, 6, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
    ] {
        let mut p = fixtures::exception_regions();
        p.functions[0].code[3] = instruction;
        bad(p, ErrorKind::ControlFlow);
    }
    let mut p = fixtures::exception_regions();
    p.functions[0].code[5].operands[0] = 1;
    bad(p, ErrorKind::Region);
    let mut p = fixtures::exception_regions();
    p.functions[0].code[4] = Instruction::new(Opcode::Return, 0, 0, 0);
    bad(p, ErrorKind::ControlFlow);
    let mut p = fixtures::exception_regions();
    p.functions[0].regions[0]
        .catch
        .as_mut()
        .unwrap()
        .range
        .start = Pc(1);
    bad(p, ErrorKind::Region);
    let mut p = fixtures::helper_native();
    p.functions[0].code.pop();
    bad(p, ErrorKind::ControlFlow);
    let mut p = fixtures::helper_native();
    p.functions[0].code[0] = Instruction::new(Opcode::Rethrow, 0, 0, 0);
    bad(p, ErrorKind::ControlFlow);
}
#[test]
fn exceptional_edges_skip_consumed_catch_and_finally_handlers() {
    let p = verify(fixtures::exception_regions(), limits()).unwrap();
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(1), None).unwrap(),
        Some(HandlerTarget {
            region: RegionId(0),
            kind: HandlerKind::Catch,
            pc: Pc(2),
            exception: Some(Reg(1))
        })
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(2), None).unwrap(),
        Some(HandlerTarget {
            region: RegionId(0),
            kind: HandlerKind::Finally,
            pc: Pc(4),
            exception: None
        })
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(4), None).unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(1), Some(RegionId(0)))
            .unwrap(),
        None
    );
    assert_eq!(Opcode::EndFinally.spec().unwind, Unwind::ResumeActive);
    let mut nested = fixtures::exception_regions();
    let f = &mut nested.functions[0];
    f.code
        .insert(0, Instruction::new(Opcode::LoadConst, 0, 0, 0));
    for i in &mut f.code {
        if i.opcode == Opcode::Leave as u16 {
            i.operands[0] += 1;
        }
    }
    f.regions[0] = ExceptionRegion {
        protected: PcRange {
            start: Pc(1),
            end: Pc(3),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(3),
                end: Pc(5),
            },
            exception: Reg(1),
        }),
        finally: Some(PcRange {
            start: Pc(5),
            end: Pc(7),
        }),
    };
    f.regions.push(ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(8),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(8),
                end: Pc(9),
            },
            exception: Reg(1),
        }),
        finally: None,
    });
    f.code.push(Instruction::new(Opcode::Return, 1, 0, 0));
    f.source_map.clear();
    let p = verify(nested, limits()).unwrap();
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(5), None)
            .unwrap()
            .unwrap()
            .region,
        RegionId(1)
    );
}
#[test]
fn runtime_must_explicitly_support_all_opcodes_natives_and_capabilities() {
    let p = verify(fixtures::helper_native(), limits()).unwrap();
    assert_eq!(
        p.require_runtime(&RuntimeSupport::default())
            .unwrap_err()
            .kind,
        ErrorKind::Capability
    );
    let mut runtime = RuntimeSupport {
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: p.package().signatures[0].clone(),
        }],
    };
    p.require_runtime(&runtime).unwrap();
    runtime.natives[0].abi_version = 2;
    assert!(p.require_runtime(&runtime).is_err());
    runtime.natives[0].abi_version = 1;
    runtime.natives[0].signature.parameters[0].mutable = true;
    assert!(p.require_runtime(&runtime).is_err());
    runtime.natives[0].signature = p.package().signatures[0].clone();
    runtime.opcodes.retain(|op| *op != Opcode::Call);
    let e = p.require_runtime(&runtime).unwrap_err();
    assert_eq!(
        (e.function, e.pc, e.source),
        (
            Some(FunctionId(0)),
            Some(Pc(1)),
            p.source_at(FunctionId(0), Pc(1))
        )
    );
}
#[test]
fn pattern_outputs_and_argument_order_remain_runtime_conditional() {
    let mut p = fixtures::helper_native();
    p.functions[0].register_count = 4;
    let pattern = PatternDesc {
        nodes: vec![PatternNode::Bind {
            name: SymbolId(4),
            output: Reg(3),
        }],
        root: 0,
    };
    p.functions[0]
        .descriptors
        .push(Descriptor::Pattern(pattern));
    let i = Instruction::new(Opcode::Match, 2, 0, 1);
    let mut access = Vec::new();
    Opcode::Match
        .spec()
        .visit_registers(&p.functions[0], &i, |reg, kind| access.push((reg, kind)))
        .unwrap();
    assert!(access.contains(&(Reg(3), Access::ConditionalWrite)));
    assert!(!access.contains(&(Reg(3), Access::Write)));
    assert_eq!(
        Opcode::ArgsNamed.spec().semantic_rule,
        SemanticRule::OrderedArguments
    );
    assert_eq!(
        Opcode::CellWrite.spec().semantic_rule,
        SemanticRule::BindingWrite
    );
    assert_eq!(
        Opcode::MemberGet.spec().semantic_rule,
        SemanticRule::MemberLookup
    );
    let duplicate = verify(fixtures::duplicate_arguments(), limits()).unwrap();
    let ops: Vec<_> = duplicate.package().functions[0]
        .code
        .iter()
        .map(|i| instruction_spec(i.opcode).unwrap().opcode)
        .collect();
    assert_eq!(
        &ops[2..7],
        &[
            Opcode::ArgsNamed,
            Opcode::ArgsNamed,
            Opcode::Call,
            Opcode::ArgsPush,
            Opcode::Call
        ]
    );
}
#[test]
fn source_spans_preserve_utf8_byte_boundaries_and_locate_bad_instructions() {
    let mut p = fixtures::helper_native();
    p.sources[0].text = "中文🎬".into();
    p.functions[1].source_map.clear();
    p.functions[0].source_map = vec![SourceMapEntry {
        pc: Pc(0),
        span: SourceSpan {
            file: SourceId(0),
            start: 0,
            end: 6,
        },
    }];
    let bytes = encode(&p, limits()).unwrap();
    let v = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(v.source_at(FunctionId(0), Pc(0)).unwrap().end, 6);
    p.functions[0].source_map[0].span.end = 1;
    bad(p, ErrorKind::Source);
    let mut p = fixtures::helper_native();
    p.functions[0].code[1].operands[0] = u32::MAX;
    let expected = p.functions[0].source_map[1].span;
    let e = verify(p, limits()).unwrap_err();
    assert_eq!((e.pc, e.source), (Some(Pc(1)), Some(expected)));
}

fn gallery(op: Opcode) -> BytecodePackage {
    let mut p = fixtures::helper_native();
    p.modules[0].globals.push(GlobalDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        linked_function: None,
    });
    let f = &mut p.functions[0];
    f.register_count = 4;
    f.captures = vec![CaptureKind::Value];
    f.source_map.clear();
    f.bindings = vec![BindingDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        storage: BindingStorage::Cell(Reg(0)),
        initialization: BindingInitialization::Explicit,
    }];
    let call = CallDesc {
        target: CallTarget::Function(FunctionId(1)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(1),
            len: 1,
        }),
        names: vec![],
    };
    f.descriptors = vec![
        Descriptor::Global {
            module: ModuleId(0),
            global: GlobalId(0),
        },
        Descriptor::Closure {
            function: FunctionId(1),
            captures: vec![],
        },
        Descriptor::Call(call.clone()),
        Descriptor::Sequence(RegWindow {
            start: Reg(1),
            len: 2,
        }),
        Descriptor::Table(vec![[Reg(1), Reg(2)]]),
        Descriptor::Class(ClassDesc {
            base: Some(Reg(1)),
            fields: vec![ClassField {
                name: SymbolId(12),
                ty: TypeId(1),
                mutable: false,
                is_static: false,
                initializer: None,
                captures: vec![],
            }],
            methods: vec![ClassMethod {
                name: SymbolId(13),
                function: FunctionId(1),
                is_static: true,
                captures: vec![],
            }],
        }),
        Descriptor::Super {
            base: Reg(1),
            member: SymbolId(12),
        },
        Descriptor::Property {
            key: Reg(1),
            writable: true,
        },
        Descriptor::Pattern(PatternDesc {
            nodes: vec![
                PatternNode::Bind {
                    name: SymbolId(12),
                    output: Reg(2),
                },
                PatternNode::Bind {
                    name: SymbolId(12),
                    output: Reg(2),
                },
                PatternNode::Or { left: 0, right: 1 },
                PatternNode::Tuple {
                    items: vec![2],
                    rest: None,
                },
                PatternNode::Array {
                    items: vec![3],
                    rest: None,
                },
                PatternNode::Object {
                    fields: vec![PatternField {
                        name: SymbolId(13),
                        node: 4,
                        default: None,
                        captures: vec![],
                    }],
                    rest: None,
                },
                PatternNode::Is {
                    node: 5,
                    ty: TypeId(0),
                },
                PatternNode::Literal(ConstId(1)),
                PatternNode::Wildcard,
            ],
            root: 6,
        }),
        Descriptor::Spawn {
            call,
            owner: Reg(3),
        },
    ];
    let operands = op.spec().operands.map(|kind| match kind {
        OperandKind::ReadReg | OperandKind::ReadWriteReg => 1,
        OperandKind::WriteReg => 0,
        OperandKind::Const | OperandKind::Type => 1,
        OperandKind::Symbol => 4,
        OperandKind::Pc => 1,
        OperandKind::Descriptor(kind) => {
            f.descriptors.iter().position(|d| d.kind() == kind).unwrap() as u32
        }
        _ => 0,
    });
    f.code = vec![
        Instruction::new(op, operands[0], operands[1], operands[2]),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    if op == Opcode::Yield {
        f.kind = FunctionKind::Generator;
    }
    p
}
#[test]
fn every_opcode_and_descriptor_family_has_a_real_wire_and_disassembly_contract() {
    let mut previous = 0;
    for spec in INSTRUCTION_SPECS {
        assert!((spec.opcode as u16) > previous);
        previous = spec.opcode as u16;
        assert_eq!(instruction_spec(previous).unwrap().mnemonic, spec.mnemonic);
        if matches!(
            spec.opcode,
            Opcode::Rethrow | Opcode::Leave | Opcode::EndFinally
        ) {
            continue;
        }
        let p = gallery(spec.opcode);
        let bytes = encode(&p, limits()).unwrap_or_else(|e| panic!("{}: {e}", spec.mnemonic));
        let loaded = VerifiedProgram::decode(&bytes, limits()).unwrap();
        assert_eq!(&p, loaded.package());
        let mut text = String::new();
        disassemble(&loaded, &mut text).unwrap();
        assert!(text.contains(spec.mnemonic));
    }
    let mut p = fixtures::exception_regions();
    p.functions[0].code[2] = Instruction::new(Opcode::Rethrow, 0, 0, 0);
    verify(p, limits()).unwrap();
}
#[test]
fn invalid_pattern_graph_or_binding_sets_are_rejected_without_recursion() {
    for pattern in [
        PatternDesc {
            nodes: vec![PatternNode::Is {
                node: 0,
                ty: TypeId(0),
            }],
            root: 0,
        },
        PatternDesc {
            nodes: vec![
                PatternNode::Bind {
                    name: SymbolId(12),
                    output: Reg(2),
                },
                PatternNode::Bind {
                    name: SymbolId(13),
                    output: Reg(3),
                },
                PatternNode::Or { left: 0, right: 1 },
            ],
            root: 2,
        },
        PatternDesc {
            nodes: vec![
                PatternNode::Bind {
                    name: SymbolId(12),
                    output: Reg(2),
                },
                PatternNode::Tuple {
                    items: vec![0, 0],
                    rest: None,
                },
            ],
            root: 1,
        },
    ] {
        let mut p = gallery(Opcode::Match);
        p.functions[0].descriptors[8] = Descriptor::Pattern(pattern);
        assert!(verify(p, limits()).is_err());
    }
    let mut p = gallery(Opcode::Match);
    if let Descriptor::Pattern(pattern) = &mut p.functions[0].descriptors[8] {
        pattern.root = u32::MAX;
    }
    bad(p, ErrorKind::Index);
}
#[test]
fn decoder_capacity_accounting_matches_actual_allocator_residency() {
    for (name, constructor, _) in FIXTURES {
        let bytes = encode(&constructor(), limits()).unwrap();
        let mut decoded = None;
        let allocation = allocation_counter::measure(|| {
            decoded = Some(decode(&bytes, limits()).unwrap());
        });
        let program = verify(decoded.unwrap(), limits()).unwrap();
        let expected =
            program.stats().owned_capacity_bytes - std::mem::size_of::<BytecodePackage>();
        assert_eq!(
            allocation.bytes_current as usize, expected,
            "retained capacity {name}"
        );
        assert!(
            allocation.bytes_max as usize <= expected + 4 * program.package().modules.capacity()
        );
        println!(
            "allocator {name}: encoded={} heap={} peak={} total={} allocations={}",
            bytes.len(),
            allocation.bytes_current,
            allocation.bytes_max,
            allocation.bytes_total,
            allocation.count_total
        );
    }
}
#[test]
fn hostile_field_mutations_never_panic_or_publish_invalid_programs() {
    let bytes = encode(&fixtures::helper_native(), limits()).unwrap();
    for offset in 0..bytes.len() {
        for value in [0, 1, 0x7f, 0xff] {
            let mut mutated = bytes.clone();
            mutated[offset] = value;
            let result = std::panic::catch_unwind(|| VerifiedProgram::decode(&mutated, limits()));
            assert!(result.is_ok(), "panic at byte {offset}={value}");
            if let Ok(program) = result.unwrap() {
                let canonical = encode(program.package(), limits()).unwrap();
                assert_eq!(canonical, mutated);
            }
        }
    }
}

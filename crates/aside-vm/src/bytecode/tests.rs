use super::*;

type Fixture = (&'static str, fn() -> BytecodePackage, &'static [u8]);

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
        VerifiedProgram::decode(&bytes, limits()).unwrap().package(),
        v.package()
    );
    let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
        unreachable!()
    };
    call.names[0].name = rest_name;
    let e = verify(p, limits()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Signature);
    assert!(e.detail.contains("cannot supply rest"));
}
const FIXTURES: &[Fixture] = &[
    (
        "native-parameter-defaults",
        fixtures::native_parameter_defaults,
        include_bytes!("testdata/native-parameter-defaults.ab"),
    ),
    (
        "parameter-cell-default",
        fixtures::parameter_cell_default,
        include_bytes!("testdata/parameter-cell-default.ab"),
    ),
    (
        "initialized-null-cell",
        fixtures::initialized_null_cell,
        include_bytes!("testdata/initialized-null-cell.ab"),
    ),
    (
        "uninitialized-cell-read",
        fixtures::uninitialized_cell_read,
        include_bytes!("testdata/uninitialized-cell-read.ab"),
    ),
    (
        "second-cell-initialization",
        fixtures::second_cell_initialization,
        include_bytes!("testdata/second-cell-initialization.ab"),
    ),
    (
        "readonly-cell-write",
        fixtures::readonly_cell_write,
        include_bytes!("testdata/readonly-cell-write.ab"),
    ),
    (
        "forward-readonly-cell",
        fixtures::forward_readonly_cell,
        include_bytes!("testdata/forward-readonly-cell.ab"),
    ),
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
    p.functions[0].code[1].operands[0] = 99;
    bad(p, ErrorKind::Index);
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
            signature: Signature {
                parameters: vec![Parameter {
                    name: SymbolId(4),
                    ty: TypeId(1),
                    mutable: false,
                    default: None,
                }],
                rest: None,
                return_type: TypeId(1),
                effects: Effects::ALL,
                property_bag_tail: None,
            },
            required_capabilities: vec!["host.echo".into()],
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
        let expected = program.stats().owned_capacity_bytes
            - program.stats().derived_capacity_bytes
            - std::mem::size_of::<BytecodePackage>();
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
#[test]
fn forward_readonly_cell_carries_initialization_and_write_contract() {
    let p = verify(fixtures::forward_readonly_cell(), limits()).unwrap();
    let f = &p.package().functions[0];
    assert_eq!(
        f.bindings[0].initialization,
        BindingInitialization::Explicit
    );
    assert!(!f.bindings[0].mutable);
    assert_eq!(f.code[0], Instruction::new(Opcode::CellNew, 0, 0, 0));
    assert_eq!(f.code[1].opcode, Opcode::MakeClosure as u16);
    assert_eq!(f.code[3].opcode, Opcode::BindInit as u16);
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
        .visit_registers(f, &f.code[0], |r, a| register_access.push((r, a)))
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
        TypeDef::Function(SignatureId(0)),
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
        VerifiedProgram::decode(&bytes, limits()).unwrap().package(),
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
    p.types.push(TypeDef::Function(SignatureId(2)));
    p.signatures[2].return_type = TypeId(6);
    bad(p, ErrorKind::Signature);
}
#[test]
fn nested_try_can_leave_inside_finally_but_cannot_escape_it() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Leave, 6, 0, 0),
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::Leave, 5, 0, 0),
        Instruction::new(Opcode::Leave, 5, 0, 0),
        Instruction::new(Opcode::EndFinally, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(6),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(2),
                end: Pc(4),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(4),
                    end: Pc(5),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    let program = verify(p.clone(), limits()).unwrap();
    assert_eq!(
        program
            .cleanup_handler(FunctionId(0), Pc(3), CleanupDestination::Leave(Pc(5)), None)
            .unwrap(),
        None,
        "an inner Leave preserves the already active outer finally"
    );
    p.functions[0].code[3].operands[0] = 6;
    bad(p, ErrorKind::ControlFlow);
}
#[test]
fn small_encoded_packages_cannot_spend_large_declared_window_work() {
    let mut p = gallery(Opcode::MakeArray);
    p.functions[0].register_count = 1_000_000_000;
    p.functions[0].descriptors[3] = Descriptor::Sequence(RegWindow {
        start: Reg(0),
        len: 1_000_000_000,
    });
    let bytes = super::codec::encode_unverified(&p);
    assert!(bytes.len() < 2048);
    let small = Limits {
        max_registers: 1_000_000_000,
        max_work: 20_000,
        ..limits()
    };
    let began = std::time::Instant::now();
    assert_eq!(
        VerifiedProgram::decode(&bytes, small).unwrap_err().kind,
        ErrorKind::Limit
    );
    assert!(
        began.elapsed() < std::time::Duration::from_secs(2),
        "declared window traversal preceded work rejection"
    );
    let mut spent = 0;
    let mut visited = 0;
    let result = Opcode::MakeArray.spec().visit_registers_budgeted(
        &p.functions[0],
        &p.functions[0].code[0],
        |units| {
            spent += units;
            if spent > 10 {
                Err(Error::new(ErrorKind::Limit, "test work"))
            } else {
                Ok(())
            }
        },
        |_, _| visited += 1,
    );
    assert_eq!(result.unwrap_err().kind, ErrorKind::Limit);
    assert_eq!(
        visited, 1,
        "only direct destination is visited before window precharge"
    );
}
#[test]
fn unwind_cursor_moves_strictly_outward_across_two_finally_layers() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 1, 0, 0),
        Instruction::new(Opcode::EndFinally, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 1, 0, 0),
        Instruction::new(Opcode::EndFinally, 1, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(4),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(4),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(4),
                end: Pc(6),
            }),
        },
    ];
    let bytes = encode(&p, limits()).unwrap();
    let p = VerifiedProgram::decode(&bytes, limits()).unwrap();
    let inner = p
        .exception_handler(FunctionId(0), Pc(1), None)
        .unwrap()
        .unwrap();
    assert_eq!(inner.region, RegionId(0));
    let outer = p
        .exception_handler(FunctionId(0), Pc(1), Some(inner.region))
        .unwrap()
        .unwrap();
    assert_eq!(outer.region, RegionId(1));
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(1), Some(outer.region))
            .unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(4), Some(inner.region))
            .unwrap_err()
            .kind,
        ErrorKind::Region
    );
}
#[test]
fn protected_and_handler_blocks_cannot_have_different_lexical_parents() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::Jump, 2, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
        Instruction::new(Opcode::Leave, 4, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(3),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(3),
                    end: Pc(4),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(4),
                end: Pc(6),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(1),
                    end: Pc(2),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    bad(p, ErrorKind::Region);
}
#[test]
fn native_requirements_cannot_be_erased_from_untrusted_package_metadata() {
    let mut package = fixtures::helper_native();
    package.capabilities.clear();
    package.natives[0].capabilities.clear();
    let program = verify(package, limits()).unwrap();
    let mut trusted = RuntimeSupport {
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec![],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            required_capabilities: vec!["host.echo".into()],
            signature: Signature {
                parameters: vec![Parameter {
                    name: SymbolId(4),
                    ty: TypeId(1),
                    mutable: false,
                    default: None,
                }],
                rest: None,
                return_type: TypeId(1),
                effects: Effects::ALL,
                property_bag_tail: None,
            },
        }],
    };
    assert_eq!(
        program.require_runtime(&trusted).unwrap_err().kind,
        ErrorKind::Capability
    );
    trusted.capabilities.push("host.echo".into());
    assert_eq!(
        program.require_runtime(&trusted).unwrap_err().kind,
        ErrorKind::Capability
    );
    let correct = verify(fixtures::helper_native(), limits()).unwrap();
    correct.require_runtime(&trusted).unwrap();
}

fn budget_package(parameters: u32, extra_symbols: u32) -> BytecodePackage {
    let mut p = BytecodePackage {
        symbols: vec!["budget::fixture".into(), "function".into()],
        types: vec![TypeDef::Builtin(BuiltinType::Any)],
        modules: vec![ModuleDef {
            path: SymbolId(0),
            imports: vec![],
            exports: vec![],
            globals: vec![],
            initializer: None,
        }],
        ..BytecodePackage::default()
    };
    p.symbols.extend((0..parameters).map(|i| format!("p{i}")));
    p.symbols
        .extend((0..extra_symbols).map(|i| format!("n{i}")));
    p.signatures.push(Signature {
        parameters: (0..parameters)
            .map(|i| Parameter {
                name: SymbolId(i + 2),
                ty: TypeId(0),
                mutable: false,
                default: None,
            })
            .collect(),
        rest: None,
        return_type: TypeId(0),
        effects: Effects(0),
        property_bag_tail: None,
    });
    p
}
fn budget_function(signature: u32, registers: u32, captures: usize) -> FunctionDef {
    FunctionDef {
        module: ModuleId(0),
        name: SymbolId(1),
        kind: FunctionKind::Plain,
        signature: SignatureId(signature),
        context: Context::Ordinary,
        captures: vec![CaptureKind::Cell; captures],
        register_count: registers.max(1),
        bindings: vec![],
        descriptors: vec![],
        code: vec![Instruction::new(Opcode::Return, 0, 0, 0)],
        regions: vec![],
        source_map: vec![],
    }
}
fn assert_shared_scan_limit(p: BytecodePackage, max_work: u64) {
    let bytes = super::codec::encode_unverified(&p);
    let small = Limits {
        max_work,
        ..limits()
    };
    // The low limit must reach semantic verification, not fail byte decoding.
    decode(&bytes, small).unwrap();
    assert_eq!(verify(p, small).unwrap_err().kind, ErrorKind::Limit);
    assert_eq!(
        VerifiedProgram::decode(&bytes, small).unwrap_err().kind,
        ErrorKind::Limit
    );
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
        p.signatures[0].parameters[i as usize].default = Some(FunctionId(FUNCTIONS + i));
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
    let bytes = super::codec::encode_unverified(&p);
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
fn runtime_admission_uses_the_saved_work_limit_for_registry_scans() {
    let program = verify(
        fixtures::helper_native(),
        Limits {
            max_work: 10_000,
            ..limits()
        },
    )
    .unwrap();
    let mut support = RuntimeSupport {
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            symbol: "host::echo".into(),
            abi_version: 1,
            signature: program.package().signatures[0].clone(),
            required_capabilities: vec!["host.echo".into()],
        }],
    };
    program.require_runtime(&support).unwrap();
    support.capabilities = (0..2000).map(|i| format!("unrelated.{i}")).collect();
    support.capabilities.push("host.echo".into());
    assert_eq!(
        program.require_runtime(&support).unwrap_err().kind,
        ErrorKind::Limit
    );
}

fn ordinary_large_package(functions: u32) -> BytecodePackage {
    let mut p = budget_package(0, 0);
    p.constants.push(Const::Null);
    for i in 0..functions {
        p.symbols.push(format!("f{i:04x}"));
        let mut f = budget_function(0, 1, 0);
        f.name = SymbolId(i + 2);
        f.code
            .insert(0, Instruction::new(Opcode::LoadConst, 0, 0, 0));
        p.functions.push(f);
        p.modules[0].globals.push(GlobalDef {
            name: SymbolId(i + 2),
            ty: TypeId(0),
            mutable: false,
            linked_function: Some(FunctionId(i)),
        });
    }
    p
}

#[test]
fn ordinary_project_loading_scales_under_default_limits() {
    let support = RuntimeSupport {
        opcodes: INSTRUCTION_SPECS.iter().map(|s| s.opcode).collect(),
        natives: vec![],
        capabilities: vec![],
    };
    for count in [1000, 5000, 10_000] {
        let p = ordinary_large_package(count);
        let bytes = super::codec::encode_unverified(&p);
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
                opcodes: [Opcode::LoadConst, Opcode::Return].repeat(5000),
                natives: vec![],
                capabilities: vec![],
            })
            .unwrap();
    }
}

fn cleanups_nested_in_catch() -> BytecodePackage {
    let mut p = budget_package(0, 0);
    p.constants.push(Const::Null);
    let mut f = budget_function(0, 1, 0);
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
        Instruction::new(Opcode::Leave, 9, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::EndFinally, 2, 0, 0),
        Instruction::new(Opcode::Leave, 12, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::EndFinally, 1, 0, 0),
        Instruction::new(Opcode::Leave, 16, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::EndFinally, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    let range = |start, end| PcRange {
        start: Pc(start),
        end: Pc(end),
    };
    f.regions = vec![
        ExceptionRegion {
            protected: range(0, 2),
            catch: Some(CatchRegion {
                range: range(2, 14),
                exception: Reg(0),
            }),
            finally: Some(range(14, 16)),
        },
        ExceptionRegion {
            protected: range(3, 10),
            catch: None,
            finally: Some(range(10, 12)),
        },
        ExceptionRegion {
            protected: range(4, 7),
            catch: None,
            finally: Some(range(7, 9)),
        },
    ];
    p.functions.push(f);
    p
}

#[test]
fn return_cleanup_uses_catch_nesting_and_strict_consumed_ancestors() {
    let bytes = encode(&cleanups_nested_in_catch(), limits()).unwrap();
    let p = VerifiedProgram::decode(&bytes, limits()).unwrap();
    let mut consumed = None;
    for (region, pc) in [(2, 7), (1, 10), (0, 14)] {
        let target = p
            .cleanup_handler(FunctionId(0), Pc(4), CleanupDestination::Return, consumed)
            .unwrap()
            .unwrap();
        assert_eq!(
            target,
            HandlerTarget {
                region: RegionId(region),
                kind: HandlerKind::Finally,
                pc: Pc(pc),
                exception: None,
            }
        );
        consumed = Some(target.region);
    }
    assert_eq!(
        p.cleanup_handler(FunctionId(0), Pc(4), CleanupDestination::Return, consumed)
            .unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), Pc(4), Some(RegionId(2)))
            .unwrap()
            .unwrap()
            .region,
        RegionId(1)
    );
    assert_eq!(
        p.cleanup_handler(
            FunctionId(0),
            Pc(13),
            CleanupDestination::Return,
            Some(RegionId(2))
        )
        .unwrap_err()
        .kind,
        ErrorKind::Region
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(0), Pc(16), CleanupDestination::Return, None)
            .unwrap(),
        None
    );
}

#[test]
fn leave_cleanup_preserves_enclosing_catch_and_validates_destination() {
    let p = verify(cleanups_nested_in_catch(), limits()).unwrap();
    for (source, destination, region) in [(5, 9, 2), (9, 12, 1), (12, 16, 0)] {
        let exit = CleanupDestination::Leave(Pc(destination));
        assert_eq!(
            p.cleanup_handler(FunctionId(0), Pc(source), exit, None)
                .unwrap()
                .unwrap()
                .region,
            RegionId(region)
        );
        assert_eq!(
            p.cleanup_handler(FunctionId(0), Pc(source), exit, Some(RegionId(region)))
                .unwrap(),
            None
        );
    }
    assert_eq!(
        p.cleanup_handler(
            FunctionId(0),
            Pc(12),
            CleanupDestination::Leave(Pc(17)),
            None
        )
        .unwrap_err()
        .kind,
        ErrorKind::Index
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(0), Pc(17), CleanupDestination::Return, None)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(1), Pc(4), CleanupDestination::Return, None)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
}

#[test]
fn parameter_cell_default_is_an_internal_prefix_and_an_ordinary_call_target() {
    let p = fixtures::parameter_cell_default();
    let bytes = encode(&p, limits()).unwrap();
    let program = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(program.package(), &p);
    assert_eq!(p.signatures[1].parameters[1].default, Some(FunctionId(2)));
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
    let bytes = super::codec::encode_unverified(&p);
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
            VerifiedProgram::decode(&bytes, limits()).unwrap().package(),
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
        VerifiedProgram::decode(&bytes, limits()).unwrap().package(),
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
        p.signatures[0].parameters[i as usize].default = Some(FunctionId(FUNCTIONS + i));
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
    let bytes = super::codec::encode_unverified(&p);
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
    assert_eq!(program.package(), &p);
    assert!(program.stats().validation_scratch_peak <= p.symbols.len() * 16);
}

#[test]
fn native_defaults_publish_one_canonical_sparse_layout_without_wire_fields() {
    let p = fixtures::native_parameter_defaults();
    let bytes = encode(&p, limits()).unwrap();
    let program = VerifiedProgram::decode(&bytes, limits()).unwrap();
    assert_eq!(program.package(), &p);
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
    assert_eq!(program.stats().derived_capacity_bytes, 2 * 8);
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
    assert_eq!(ordinary.stats().derived_capacity_bytes, 0);
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
    assert_eq!(ordinary.stats().derived_capacity_bytes, 0);
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

#[test]
fn verified_native_cache_capacity_matches_actual_retained_allocation() {
    let bytes = encode(&fixtures::native_parameter_defaults(), limits()).unwrap();
    let mut loaded = None;
    let allocation = allocation_counter::measure(|| {
        loaded = Some(VerifiedProgram::decode(&bytes, limits()).unwrap());
    });
    let program = loaded.unwrap();
    let stats = program.stats();
    assert_eq!(
        allocation.bytes_current as usize,
        stats.owned_capacity_bytes - std::mem::size_of::<BytecodePackage>()
    );
    assert_eq!(stats.derived_capacity_bytes, 16);
    assert!(
        allocation.bytes_max as usize
            <= stats.owned_capacity_bytes - std::mem::size_of::<BytecodePackage>()
                + stats.validation_scratch_peak
    );
}

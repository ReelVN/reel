use super::*;

fn pc(program: &VerifiedProgram, ordinal: u32) -> Pc {
    let code = &program.package().functions[0].code;
    Pc(code
        .indexed_iter()
        .nth(ordinal as usize)
        .transpose()
        .unwrap()
        .map_or_else(
            || code.end_pc() + (ordinal as usize).saturating_sub(code.len()),
            |(pc, _)| pc,
        ) as u32)
}

fn code_section(words: &[u32]) -> Vec<u8> {
    use super::code_residency::PAGE_WORDS;
    let mut blocks = Vec::new();
    let mut packed = Vec::new();
    for page in words.chunks(PAGE_WORDS) {
        let raw: Vec<_> = page.iter().flat_map(|word| word.to_le_bytes()).collect();
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        blocks.push((packed.len() as u32, data.len() as u32));
        packed.extend(data);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(words.len() as u32).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for (offset, len) in blocks {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
    }
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend(packed);
    out
}
fn rewrite_execution_words(bytes: &[u8], edit: impl FnOnce(&mut [u32])) -> Vec<u8> {
    let p = decode(bytes, limits()).unwrap();
    let store = p.functions[0].code.runtime_store().unwrap();
    let mut words: Vec<_> = store
        .words(0, store.words_len())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let before = code_section(&words);
    let offset = bytes
        .windows(before.len())
        .position(|part| part == before)
        .unwrap();
    edit(&mut words);
    let after = code_section(&words);
    let mut corrupt = bytes.to_vec();
    corrupt.splice(offset..offset + before.len(), after);
    let len = corrupt.len() as u32;
    corrupt[28..32].copy_from_slice(&len.to_le_bytes());
    corrupt
}

fn parameter_layout_bytes(program: &VerifiedProgram) -> usize {
    let instructions = program
        .package()
        .functions
        .iter()
        .map(|f| f.code.len())
        .sum::<usize>();
    let opcode_capacity =
        instructions.min(INSTRUCTION_SPECS.len()) * size_of::<(u16, FunctionId, Pc)>();
    program.stats().derived_capacity_bytes
        - opcode_capacity
        - program.native_call_capacity_bytes()
        - program.compact_derived_capacity_bytes()
        - program.scalar_suffix_capacity_bytes()
        - program.scalar_entry_capacity_bytes()
}

type Fixture = (&'static str, fn() -> BytecodePackage, &'static [u8]);

#[test]
#[ignore = "explicit code identity throughput measurement"]
fn decoded_identity_cost() {
    let mut package = fixtures::helper_native();
    let function = &mut package.functions[0];
    function.code = vec![Instruction::new(Opcode::LoadConst, 0, 1, 0); 1_000_000];
    function
        .code
        .push(Instruction::new(Opcode::Return, 0, 0, 0));
    function.source_map.clear();
    let bytes = encode(&package, limits()).unwrap();
    let mut canonical = Vec::new();
    let mut decoded = Vec::new();
    for _ in 0..7 {
        let start = std::time::Instant::now();
        let original = super::codec::identity(std::hint::black_box(&package), limits()).unwrap();
        canonical.push(start.elapsed());
        let start = std::time::Instant::now();
        let loaded =
            super::codec::decoded_identity(std::hint::black_box(&bytes), limits()).unwrap();
        decoded.push(start.elapsed());
        assert_eq!(original, loaded);
    }
    canonical.sort();
    decoded.sort();
    let mut identity = None;
    let allocation = allocation_counter::measure(|| {
        identity = Some(super::codec::decoded_identity(&bytes, limits()).unwrap());
    });
    assert!(identity.is_some());
    assert_eq!(allocation.count_total, 0);
    eprintln!(
        "identity encoded_bytes={} instructions={} canonical_median={:?} decoded_median={:?} decoded_allocations={}",
        bytes.len(),
        package.functions[0].code.len(),
        canonical[3],
        decoded[3],
        allocation.count_total
    );
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

#[test]
fn native_callable_wire_preserves_registration_and_rejects_script_frame_metadata() {
    let mut p = fixtures::helper_native();
    let mut f = p.functions[1].clone();
    f.kind = FunctionKind::Native(NativeId(0));
    f.signature = p.natives[0].signature;
    f.context = Context::Ordinary;
    f.register_count = 0;
    f.captures.clear();
    f.bindings.clear();
    f.descriptors.clear();
    f.code.clear();
    f.regions.clear();
    f.source_map.truncate(1);
    f.source_map[0].pc = Pc(0);
    let index = p.functions.len();
    p.functions.push(f);
    let bytes = encode(&p, limits()).unwrap();
    assert_eq!(
        &VerifiedProgram::decode(&bytes, limits())
            .unwrap()
            .to_unpacked()
            .unwrap(),
        &p
    );
    for mutate in [
        |f: &mut FunctionDef| f.register_count = 1,
        |f: &mut FunctionDef| f.context = Context::NoWait,
        |f: &mut FunctionDef| f.signature = SignatureId(1),
        |f: &mut FunctionDef| f.code.push(Instruction::new(Opcode::Return, 0, 0, 0)),
        |f: &mut FunctionDef| {
            f.descriptors.push(Descriptor::Closure {
                function: fixtures::ENTRY,
                captures: vec![],
            })
        },
    ] {
        let mut invalid = p.clone();
        mutate(&mut invalid.functions[index]);
        bad(invalid, ErrorKind::Signature);
    }
    let mut invalid = p.clone();
    invalid.functions[index].kind = FunctionKind::Native(NativeId(u32::MAX));
    bad(invalid, ErrorKind::Index);
    p.functions[index].source_map[0].pc = Pc(1);
    bad(p, ErrorKind::Source);
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
        assert_eq!(decoded.to_unpacked().unwrap(), p);
        assert_eq!(encode(&decoded, limits()).unwrap(), bytes);
        let original = verify(p, limits()).unwrap();
        let loaded = VerifiedProgram::decode(&bytes, limits()).unwrap();
        assert_eq!(
            original.snapshot_identity(),
            loaded.snapshot_identity(),
            "canonical code identity {name}"
        );
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
fn instruction_wire_is_compact_and_independent_of_execution_layout() {
    let package = fixtures::helper_native();
    let bytes = encode(&package, limits()).unwrap();
    let runtime = decode(&bytes, limits()).unwrap();
    let code = &runtime.functions[0].code;
    assert_eq!(
        code.words().collect::<Result<Vec<_>, _>>().unwrap(),
        [0x101, 1, 0x841, 0x842]
    );
    let store = code.runtime_store().unwrap();
    assert!(
        bytes
            .windows(store.packed().len())
            .any(|part| part == store.packed())
    );
    assert_eq!(std::mem::size_of::<Instruction>(), 16);
    assert_eq!(
        decode(&bytes, limits()).unwrap().to_unpacked().unwrap(),
        package
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
    assert_eq!(decode(&bytes, limits()).unwrap().to_unpacked().unwrap(), p);
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
    for register in [true, false] {
        let corrupt = rewrite_execution_words(&bytes, |words| {
            if register {
                words[0] = 0x101u32 | 2 << 11;
            } else {
                words[1] = u32::MAX;
            }
        });
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                decode(&corrupt, limits()).unwrap_err().kind,
                ErrorKind::Index
            );
        });
        assert!(
            allocation.bytes_total
                < (super::code_residency::CodeStore::construction_scratch_bytes() + 2048) as u64,
            "bad index allocated package payloads"
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
        assert_eq!(verify(p.clone(), limits()).unwrap_err().kind, kind);
        assert_eq!(encode(&p, limits()).unwrap_err().kind, kind);
    }
    let mut p = fixtures::helper_native();
    p.functions[0].code[0].operands[2] = 1;
    assert_eq!(
        verify(p.clone(), limits()).unwrap_err().kind,
        ErrorKind::Encoding
    );
    assert_eq!(encode(&p, limits()).unwrap_err().kind, ErrorKind::Encoding);
}

#[test]
fn instruction_operands_reject_noncanonical_and_overflowing_encodings() {
    let bytes = encode(&fixtures::helper_native(), limits()).unwrap();
    for header in [0xffff_f801u32, 0xffff_f901, 0x501, 0x201] {
        let corrupt = rewrite_execution_words(&bytes, |words| words[0] = header);
        assert_eq!(
            decode(&corrupt, limits()).unwrap_err().kind,
            ErrorKind::Encoding
        );
    }
    let corrupt = rewrite_execution_words(&bytes, |words| words[0] = 0x1ff);
    assert_eq!(
        decode(&corrupt, limits()).unwrap_err().kind,
        ErrorKind::Opcode
    );
}

#[test]
fn optional_instruction_reads_leave_cold_execution_pages_untouched() {
    let program = verify(fixtures::helper_native(), limits()).unwrap();
    let code = &program.package().functions[0].code;
    let mut reader = ResidentCodeReader::default();
    assert_eq!(reader.read(code, 0).unwrap(), None);
    assert_eq!(
        program.code_cache_stats().unwrap().resident_capacity_bytes,
        0
    );
    let instruction = code.instruction(0).unwrap().unwrap();
    let next = code.next_pc(0).unwrap().unwrap();
    assert_eq!(reader.read(code, 0).unwrap(), Some((instruction, next)));
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
        host_types: Vec::new(),
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
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
    assert_eq!(p.code_cache_stats().unwrap().resident_capacity_bytes, 0);
    p.require_runtime(&runtime).unwrap();
    assert_eq!(
        p.code_cache_stats().unwrap().resident_capacity_bytes,
        0,
        "capability admission reads the derived opcode sites without loading code pages"
    );
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
            Some(pc(&p, 1)),
            p.source_at(FunctionId(0), pc(&p, 1))
        )
    );

    let mut package = fixtures::helper_native();
    package.functions[0].code.extend([
        Instruction::new(Opcode::MemberGet, 0, 0, 9),
        Instruction::new(Opcode::MemberGet, 0, 0, 9),
        Instruction::new(Opcode::MemberSet, 0, 0, 4),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ]);
    let bytes = encode(&package, limits()).unwrap();
    for program in [
        verify(package, limits()).unwrap(),
        VerifiedProgram::decode(&bytes, limits()).unwrap(),
    ] {
        let resident = program.code_cache_stats().unwrap().resident_capacity_bytes;
        assert_eq!(program.member_symbols(), &[SymbolId(4), SymbolId(9)]);
        assert_eq!(
            program.code_cache_stats().unwrap().resident_capacity_bytes,
            resident,
            "member names are unique metadata even when their instructions are unreachable"
        );
    }
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
        .map(Result::unwrap)
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
    let code_len = v.package().functions[0].code.end_pc();
    for item in v.package().functions[0].code.indexed_iter() {
        let (word, _) = item.unwrap();
        assert_eq!(
            v.source_at(FunctionId(0), Pc(word as u32)),
            v.source_at(FunctionId(0), Pc(0))
        );
    }
    assert_eq!(v.source_at(FunctionId(0), Pc(code_len as u32)), None);
    let mut malformed = p.clone();
    malformed.functions[0].code[1].operands[0] = u32::MAX;
    let error = verify(malformed, limits()).unwrap_err();
    assert_eq!(error.source, v.source_at(FunctionId(0), pc(&v, 1)));
    p.functions[0].source_map[0].pc = Pc(1);
    let sparse = verify(p.clone(), limits()).unwrap();
    assert_eq!(sparse.source_at(FunctionId(0), Pc(0)), None);
    assert_eq!(
        sparse.source_at(FunctionId(0), pc(&sparse, 1)),
        v.source_at(FunctionId(0), Pc(0))
    );
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
        contract: None,
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
            nominal: None,
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
    for opcode in 0..=u16::MAX {
        let declared = INSTRUCTION_SPECS
            .iter()
            .find(|spec| spec.opcode as u16 == opcode);
        assert_eq!(
            instruction_spec(opcode).map(std::ptr::from_ref),
            declared.map(std::ptr::from_ref),
            "opcode {opcode:#06x}"
        );
    }
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
        assert_eq!(&p, &loaded.to_unpacked().unwrap());
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
        let decoded = decoded.unwrap();
        let raw_owned = super::codec::measure(&decoded, limits()).unwrap().1;
        let cache = decoded.functions[0]
            .code
            .runtime_store()
            .unwrap()
            .cache_stats();
        let expected =
            raw_owned - std::mem::size_of::<RuntimePackage>() + cache.resident_capacity_bytes;
        assert_eq!(
            allocation.bytes_current as usize, expected,
            "retained capacity {name}"
        );
        assert!(
            allocation.bytes_max as usize
                <= expected + 4 * decoded.modules.capacity() + cache.construction_scratch_bytes
        );
        println!(
            "allocator {name}: encoded={} heap={} peak={} total={} allocations={}",
            bytes.len(),
            allocation.bytes_current,
            allocation.bytes_max,
            allocation.bytes_total,
            allocation.count_total
        );
        drop(decoded);
        let mut program = None;
        let retained = allocation_counter::measure(|| {
            program = Some(VerifiedProgram::decode(&bytes, limits()).unwrap());
        });
        let program = program.unwrap();
        let expected = program.stats().owned_capacity_bytes - std::mem::size_of::<RuntimePackage>()
            + program.shared_header_bytes();
        assert_eq!(
            retained.bytes_current as usize, expected,
            "compact retained capacity {name}"
        );
        assert!(
            retained.bytes_max as usize
                <= raw_owned
                    + program.stats().validation_scratch_peak
                    + program.shared_header_bytes(),
            "compact allocation peak {name}"
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
fn native_requirements_cannot_be_erased_from_untrusted_package_metadata() {
    let mut package = fixtures::helper_native();
    package.capabilities.clear();
    package.natives[0].capabilities.clear();
    let program = verify(package, limits()).unwrap();
    let mut trusted = RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec![],
        natives: vec![NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
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
        host_types: Vec::new(),
        opcodes: vec![Opcode::LoadConst, Opcode::Call, Opcode::Return],
        capabilities: vec!["host.echo".into()],
        natives: vec![NativeSupport {
            standard_derived_commit: false,
            read_only_leaf: None,
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
            + program.shared_header_bytes()
    );
    assert_eq!(parameter_layout_bytes(&program), 5 * 8 + 2 * 4);
    assert!(
        allocation.bytes_max as usize
            <= stats.owned_capacity_bytes - std::mem::size_of::<BytecodePackage>()
                + program.shared_header_bytes()
                + stats.validation_scratch_peak
    );
    let allocation = allocation_counter::measure(|| {
        let shared = program.clone();
        assert!(std::ptr::eq(shared.package(), program.package()));
    });
    assert_eq!(allocation.bytes_total, 0);
}

mod control_flow;
mod defaults;

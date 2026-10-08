//! Hand-authored packages shared by lowering and runtime contract tests.
//! No source compiler or semantic execution is claimed by these constructors.
use super::*;

pub const ENTRY: FunctionId = FunctionId(0);
pub const ECHO_NATIVE: NativeId = NativeId(0);
pub const ECHO_ARGUMENT: i64 = 41;
pub const ECHO_RESULT: i64 = 42;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoCompletion {
    Ready,
    Pending,
}
pub const ECHO_COMPLETIONS: [EchoCompletion; 2] = [EchoCompletion::Ready, EchoCompletion::Pending];

fn instruction(op: Opcode, a: u32, b: u32, c: u32) -> Instruction {
    Instruction::new(op, a, b, c)
}
fn parameter(name: u32, ty: u32, mutable: bool) -> Parameter {
    Parameter {
        name: SymbolId(name),
        ty: TypeId(ty),
        mutable,
        default: None,
    }
}
fn signature(parameters: Vec<Parameter>, return_type: u32, effects: Effects) -> Signature {
    Signature {
        parameters,
        rest: None,
        return_type: TypeId(return_type),
        effects,
        property_bag_tail: None,
    }
}
fn function(name: u32, signature: u32, registers: u32, code: Vec<Instruction>) -> FunctionDef {
    FunctionDef {
        module: ModuleId(0),
        name: SymbolId(name),
        kind: FunctionKind::Plain,
        signature: SignatureId(signature),
        context: Context::Ordinary,
        captures: Vec::new(),
        register_count: registers,
        bindings: Vec::new(),
        descriptors: Vec::new(),
        code,
        regions: Vec::new(),
        source_map: Vec::new(),
    }
}
fn base() -> BytecodePackage {
    BytecodePackage {
        symbols: [
            "game::fixture",
            "main",
            "helper",
            "host::echo",
            "x",
            "host.echo",
            "fixture.aside",
            "outer",
            "increment",
            "count",
            "default_x",
            "nested",
            "first",
            "second",
        ]
        .map(str::to_owned)
        .to_vec(),
        types: vec![
            TypeDef::Builtin(BuiltinType::Any),
            TypeDef::Builtin(BuiltinType::Int),
        ],
        constants: vec![Const::Null, Const::Int(ECHO_ARGUMENT), Const::Int(1)],
        modules: vec![ModuleDef {
            path: SymbolId(0),
            imports: Vec::new(),
            exports: Vec::new(),
            globals: Vec::new(),
            initializer: None,
        }],
        sources: vec![SourceFile {
            path: SymbolId(6),
            text: "fn main() { helper(41) }\nfn helper(x) { host::echo(x) }\n".into(),
        }],
        ..BytecodePackage::default()
    }
}
/// main -> ordinary helper -> native echo. Run the SAME package with each of
/// ECHO_COMPLETIONS. Each native entry consumes 41 exactly once and delivers 42;
/// Pending commits the same return destination after resumption, not a new call.
pub fn helper_native() -> BytecodePackage {
    let mut p = base();
    p.capabilities = vec![SymbolId(5)];
    p.signatures = vec![
        signature(vec![parameter(4, 1, false)], 1, Effects::ALL),
        signature(vec![], 1, Effects::ALL),
    ];
    p.natives = vec![NativeDef {
        symbol: SymbolId(3),
        signature: SignatureId(0),
        abi_version: 1,
        capabilities: vec![SymbolId(5)],
    }];
    let mut main = function(
        1,
        1,
        2,
        vec![
            instruction(Opcode::LoadConst, 0, 1, 0),
            instruction(Opcode::Call, 1, 0, 0),
            instruction(Opcode::Return, 1, 0, 0),
        ],
    );
    main.descriptors = vec![Descriptor::Call(CallDesc {
        target: CallTarget::Function(FunctionId(1)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 1,
        }),
        names: Vec::new(),
    })];
    main.source_map = vec![
        SourceMapEntry {
            pc: Pc(0),
            span: SourceSpan {
                file: SourceId(0),
                start: 19,
                end: 21,
            },
        },
        SourceMapEntry {
            pc: Pc(1),
            span: SourceSpan {
                file: SourceId(0),
                start: 12,
                end: 22,
            },
        },
    ];
    let mut helper = function(
        2,
        0,
        2,
        vec![
            instruction(Opcode::Call, 1, 0, 0),
            instruction(Opcode::Return, 1, 0, 0),
        ],
    );
    helper.bindings = vec![BindingDef {
        name: SymbolId(4),
        ty: TypeId(1),
        mutable: false,
        storage: BindingStorage::Value(Reg(0)),
        initialization: BindingInitialization::Parameter(0),
    }];
    helper.descriptors = vec![Descriptor::Call(CallDesc {
        target: CallTarget::Native(ECHO_NATIVE),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 1,
        }),
        names: Vec::new(),
    })];
    helper.source_map = vec![SourceMapEntry {
        pc: Pc(0),
        span: SourceSpan {
            file: SourceId(0),
            start: 39,
            end: 52,
        },
    }];
    p.functions = vec![main, helper];
    p
}
/// Two closures capture the SAME cell. The outer scope also reads that cell.
/// Each increment sees the previous mutation; captures contain no frame address.
pub fn cell_closures() -> BytecodePackage {
    let mut p = base();
    p.signatures = vec![signature(vec![], 0, Effects::ALL)];
    let mut outer = function(
        7,
        0,
        4,
        vec![
            instruction(Opcode::LoadConst, 0, 1, 0),
            instruction(Opcode::CellNew, 0, 0, 0),
            instruction(Opcode::BindInit, 0, 0, 0),
            instruction(Opcode::MakeClosure, 2, 0, 0),
            instruction(Opcode::MakeClosure, 3, 0, 0),
            instruction(Opcode::BindRead, 0, 0, 0),
            instruction(Opcode::Return, 2, 0, 0),
        ],
    );
    outer.bindings = vec![BindingDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        storage: BindingStorage::Cell(Reg(1)),
        initialization: BindingInitialization::Explicit,
    }];
    outer.descriptors = vec![Descriptor::Closure {
        function: FunctionId(1),
        captures: vec![CaptureSource::Binding(BindingId(0))],
    }];
    let mut inc = function(
        8,
        0,
        4,
        vec![
            instruction(Opcode::CaptureRead, 0, 0, 0),
            instruction(Opcode::CellRead, 1, 0, 0),
            instruction(Opcode::LoadConst, 2, 2, 0),
            instruction(Opcode::Add, 3, 1, 2),
            instruction(Opcode::CellWrite, 0, 3, 0),
            instruction(Opcode::Return, 3, 0, 0),
        ],
    );
    inc.captures = vec![CaptureKind::Cell];
    p.functions = vec![outer, inc];
    p
}
/// Protected 0..2, catch 2..4, finally 4..6. Both normal exits use Leave(6).
/// A new fault in catch routes to finally; a fault in finally skips this region.
pub fn exception_regions() -> BytecodePackage {
    let mut p = base();
    p.signatures = vec![signature(vec![], 0, Effects::ALL)];
    let mut f = function(
        1,
        0,
        2,
        vec![
            instruction(Opcode::LoadConst, 0, 1, 0),
            instruction(Opcode::Throw, 0, 0, 0),
            instruction(Opcode::Move, 0, 1, 0),
            instruction(Opcode::Leave, 6, 0, 0),
            instruction(Opcode::LoadConst, 1, 0, 0),
            instruction(Opcode::EndFinally, 0, 0, 0),
            instruction(Opcode::Return, 0, 0, 0),
        ],
    );
    f.regions = vec![ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(2),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(2),
                end: Pc(4),
            },
            exception: Reg(1),
        }),
        finally: Some(PcRange {
            start: Pc(4),
            end: Pc(6),
        }),
    }];
    f.source_map = vec![SourceMapEntry {
        pc: Pc(1),
        span: SourceSpan {
            file: SourceId(0),
            start: 0,
            end: 2,
        },
    }];
    p.functions = vec![f];
    p
}
/// Nested fn's default reads the same outer cell as its body. The default frame
/// receives the callee closure's capture environment; no external wait is legal.
pub fn captured_default() -> BytecodePackage {
    let mut p = base();
    let mut param = parameter(4, 1, false);
    param.default = Some(FunctionId(2));
    p.signatures = vec![
        signature(vec![], 0, Effects::ALL),
        signature(vec![param], 1, Effects::ALL),
        signature(vec![], 1, Effects(Effects::ALLOCATE)),
    ];
    let mut outer = function(
        7,
        0,
        3,
        vec![
            instruction(Opcode::LoadConst, 0, 1, 0),
            instruction(Opcode::CellNew, 0, 0, 0),
            instruction(Opcode::BindInit, 0, 0, 0),
            instruction(Opcode::MakeClosure, 2, 0, 0),
            instruction(Opcode::Return, 2, 0, 0),
        ],
    );
    outer.bindings = vec![BindingDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: true,
        storage: BindingStorage::Cell(Reg(1)),
        initialization: BindingInitialization::Explicit,
    }];
    outer.descriptors = vec![Descriptor::Closure {
        function: FunctionId(1),
        captures: vec![CaptureSource::Binding(BindingId(0))],
    }];
    let mut nested = function(11, 1, 1, vec![instruction(Opcode::Return, 0, 0, 0)]);
    nested.captures = vec![CaptureKind::Cell];
    let mut default = function(
        10,
        2,
        2,
        vec![
            instruction(Opcode::CaptureRead, 0, 0, 0),
            instruction(Opcode::CellRead, 1, 0, 0),
            instruction(Opcode::Return, 1, 0, 0),
        ],
    );
    default.captures = vec![CaptureKind::Cell];
    default.context = Context::NoWait;
    p.functions = vec![outer, nested, default];
    p
}
/// ArgsNamed preserves both x entries. A side-effecting echo call between the
/// second entry and final Call must run before the final binder reports duplicate.
/// This package is valid; duplicate arguments are a runtime binding fault.
pub fn duplicate_arguments() -> BytecodePackage {
    let mut p = helper_native();
    let f = &mut p.functions[0];
    f.register_count = 3;
    f.code = vec![
        instruction(Opcode::ArgsNew, 0, 0, 0),
        instruction(Opcode::LoadConst, 1, 1, 0),
        instruction(Opcode::ArgsNamed, 0, 1, 4),
        instruction(Opcode::ArgsNamed, 0, 1, 4),
        instruction(Opcode::Call, 2, 1, 0),
        instruction(Opcode::ArgsPush, 0, 2, 0),
        instruction(Opcode::Call, 2, 0, 0),
        instruction(Opcode::Return, 2, 0, 0),
    ];
    f.descriptors = vec![
        Descriptor::Call(CallDesc {
            target: CallTarget::Function(FunctionId(1)),
            arguments: Arguments::Packed(Reg(0)),
            names: vec![],
        }),
        Descriptor::Call(CallDesc {
            target: CallTarget::Native(NativeId(0)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(1),
                len: 1,
            }),
            names: vec![],
        }),
    ];
    f.source_map.clear();
    p
}

/// Forward readonly capture: closure creation precedes the binding initializer.
/// The cell exists with an unset initialization bit, then accepts BindInit once.
/// Calling the closure before that publication must raise UninitializedBinding;
/// afterwards it reads 1, and CellWrite through any alias raises ReadOnlyWrite.
pub fn forward_readonly_cell() -> BytecodePackage {
    let mut p = base();
    p.signatures = vec![
        signature(vec![], 0, Effects::ALL),
        signature(vec![], 1, Effects::ALL),
    ];
    let mut outer = function(
        7,
        0,
        3,
        vec![
            instruction(Opcode::CellNew, 0, 0, 0),
            instruction(Opcode::MakeClosure, 2, 0, 0),
            instruction(Opcode::LoadConst, 0, 2, 0),
            instruction(Opcode::BindInit, 0, 0, 0),
            instruction(Opcode::Return, 2, 0, 0),
        ],
    );
    outer.bindings = vec![BindingDef {
        name: SymbolId(9),
        ty: TypeId(1),
        mutable: false,
        storage: BindingStorage::Cell(Reg(1)),
        initialization: BindingInitialization::Explicit,
    }];
    outer.descriptors = vec![Descriptor::Closure {
        function: FunctionId(1),
        captures: vec![CaptureSource::Binding(BindingId(0))],
    }];
    let mut read = function(
        2,
        1,
        2,
        vec![
            instruction(Opcode::CaptureRead, 0, 0, 0),
            instruction(Opcode::CellRead, 1, 0, 0),
            instruction(Opcode::Return, 1, 0, 0),
        ],
    );
    read.captures = vec![CaptureKind::Cell];
    p.functions = vec![outer, read];
    p
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellOutcome {
    IntOne,
    Null,
    UninitializedReadFault,
    SecondInitializationFault,
    ReadonlyWriteFault,
}
pub struct CellCase {
    pub name: &'static str,
    pub package: fn() -> BytecodePackage,
    pub invoke_returned_closure: bool,
    pub expected: CellOutcome,
}
/// Runtime acceptance cases. B01 verifies their format; the later dispatcher
/// must assert these distinct outcomes rather than treating them as executed.
pub const CELL_CASES: &[CellCase] = &[
    CellCase {
        name: "forward-readonly-cell",
        package: forward_readonly_cell,
        invoke_returned_closure: true,
        expected: CellOutcome::IntOne,
    },
    CellCase {
        name: "initialized-null-cell",
        package: initialized_null_cell,
        invoke_returned_closure: true,
        expected: CellOutcome::Null,
    },
    CellCase {
        name: "uninitialized-cell-read",
        package: uninitialized_cell_read,
        invoke_returned_closure: false,
        expected: CellOutcome::UninitializedReadFault,
    },
    CellCase {
        name: "second-cell-initialization",
        package: second_cell_initialization,
        invoke_returned_closure: false,
        expected: CellOutcome::SecondInitializationFault,
    },
    CellCase {
        name: "readonly-cell-write",
        package: readonly_cell_write,
        invoke_returned_closure: true,
        expected: CellOutcome::ReadonlyWriteFault,
    },
];
pub fn initialized_null_cell() -> BytecodePackage {
    let mut p = forward_readonly_cell();
    p.functions[0].bindings[0].ty = TypeId(0);
    p.signatures[1].return_type = TypeId(0);
    p.functions[0].code[2] = instruction(Opcode::LoadConst, 0, 0, 0);
    p
}
pub fn uninitialized_cell_read() -> BytecodePackage {
    let mut p = forward_readonly_cell();
    p.functions[0].descriptors.push(Descriptor::Call(CallDesc {
        target: CallTarget::Register(Reg(2)),
        arguments: Arguments::Window(RegWindow {
            start: Reg(0),
            len: 0,
        }),
        names: vec![],
    }));
    p.functions[0]
        .code
        .insert(2, instruction(Opcode::Call, 0, 1, 0));
    p
}
pub fn second_cell_initialization() -> BytecodePackage {
    let mut p = forward_readonly_cell();
    p.functions[0]
        .code
        .insert(4, instruction(Opcode::BindInit, 0, 0, 0));
    p
}
pub fn readonly_cell_write() -> BytecodePackage {
    let mut p = forward_readonly_cell();
    p.functions[1]
        .code
        .insert(2, instruction(Opcode::CellWrite, 0, 1, 0));
    p
}

/// owner(mut x, y=default_y(x)) and default_y both bind x to Cell(Reg(0)).
/// The internal prefix path reuses the owner's cell; default_y increments it
/// and returns a closure, then the owner increments it again. Expected owner
/// tuple: (43, closure reading 43). Main also CALLS THE SAME default FunctionDef
/// normally with 41: that invocation owns a new cell and its closure reads 42.
/// These are runtime expectations, not semantic execution claims by B01.
pub fn parameter_cell_default() -> BytecodePackage {
    let mut p = base();
    p.symbols.extend(["y".into(), "default_y".into()]);
    p.sources[0].text = "fn owner(mut x: int, y = { x += 1; || x }) { x += 1; (x,y) }\n".into();
    let x = parameter(4, 1, true);
    let mut y = parameter(14, 0, false);
    y.default = Some(FunctionId(2));
    p.signatures = vec![
        signature(vec![], 0, Effects::ALL),
        signature(vec![x.clone(), y.clone()], 0, Effects::ALL),
        signature(vec![x], 0, Effects(Effects::ALLOCATE | Effects::WRITE)),
        signature(vec![], 1, Effects(0)),
    ];
    let parameter_binding = |name, ty, mutable, storage, i| BindingDef {
        name: SymbolId(name),
        ty: TypeId(ty),
        mutable,
        storage,
        initialization: BindingInitialization::Parameter(i),
    };
    let mut main = function(
        1,
        0,
        4,
        vec![
            instruction(Opcode::LoadConst, 0, 1, 0),
            instruction(Opcode::Call, 1, 0, 0),
            instruction(Opcode::Call, 2, 1, 0),
            instruction(Opcode::MakeTuple, 3, 2, 0),
            instruction(Opcode::Return, 3, 0, 0),
        ],
    );
    main.descriptors = vec![
        Descriptor::Call(CallDesc {
            target: CallTarget::Function(FunctionId(1)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(0),
                len: 1,
            }),
            names: vec![],
        }),
        Descriptor::Call(CallDesc {
            target: CallTarget::Function(FunctionId(2)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(0),
                len: 1,
            }),
            names: vec![],
        }),
        Descriptor::Sequence(RegWindow {
            start: Reg(1),
            len: 2,
        }),
    ];
    let mut owner = function(
        2,
        1,
        5,
        vec![
            instruction(Opcode::BindRead, 2, 0, 0),
            instruction(Opcode::LoadConst, 3, 2, 0),
            instruction(Opcode::Add, 4, 2, 3),
            instruction(Opcode::BindWrite, 0, 4, 0),
            instruction(Opcode::BindRead, 2, 0, 0),
            instruction(Opcode::BindRead, 3, 1, 0),
            instruction(Opcode::MakeTuple, 4, 0, 0),
            instruction(Opcode::Return, 4, 0, 0),
        ],
    );
    owner.bindings = vec![
        parameter_binding(4, 1, true, BindingStorage::Cell(Reg(0)), 0),
        parameter_binding(14, 0, false, BindingStorage::Value(Reg(1)), 1),
    ];
    owner.descriptors = vec![Descriptor::Sequence(RegWindow {
        start: Reg(2),
        len: 2,
    })];
    let mut default = function(
        15,
        2,
        5,
        vec![
            instruction(Opcode::BindRead, 1, 0, 0),
            instruction(Opcode::LoadConst, 2, 2, 0),
            instruction(Opcode::Add, 3, 1, 2),
            instruction(Opcode::BindWrite, 0, 3, 0),
            instruction(Opcode::MakeClosure, 4, 0, 0),
            instruction(Opcode::Return, 4, 0, 0),
        ],
    );
    default.context = Context::NoWait;
    default.bindings = vec![parameter_binding(
        4,
        1,
        true,
        BindingStorage::Cell(Reg(0)),
        0,
    )];
    default.descriptors = vec![Descriptor::Closure {
        function: FunctionId(3),
        captures: vec![CaptureSource::Binding(BindingId(0))],
    }];
    let mut read = function(
        8,
        3,
        2,
        vec![
            instruction(Opcode::CaptureRead, 0, 0, 0),
            instruction(Opcode::CellRead, 1, 0, 0),
            instruction(Opcode::Return, 1, 0, 0),
        ],
    );
    read.captures = vec![CaptureKind::Cell];
    p.functions = vec![main, owner, default, read];
    p
}

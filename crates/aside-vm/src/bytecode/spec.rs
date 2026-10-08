use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperandKind {
    Unused,
    ReadReg,
    WriteReg,
    ReadWriteReg,
    Const,
    Symbol,
    Type,
    Binding,
    BindingRead,
    BindingWrite,
    BindingInit,
    Capture,
    Pc,
    Region,
    Descriptor(DescriptorKind),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Next,
    Jump,
    Branch,
    Return,
    Throw,
    Leave,
    EndFinally,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suspension {
    None,
    Wait,
    Yield,
    Thread,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
    ConditionalWrite,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unwind {
    None,
    Raise,
    Exit,
    ResumeActive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRule {
    Ordinary,
    /// One-time initialization, validating the BindingDef type contract.
    BindingInit,
    /// Type, initialized state and mutable permission apply to all aliases.
    BindingWrite,
    /// Creates an uninitialized cell carrying the referenced BindingDef contract.
    CellContract,
    /// Append ordered entries, preserving duplicates until final call binding.
    OrderedArguments,
    /// Copy elements/entries at this instruction, before later source side effects.
    SpreadSnapshot,
    /// Output bindings become initialized atomically on successful matching only.
    PatternCommit,
    /// Bind receiver for class methods only; table closures remain plain functions.
    MemberLookup,
    /// Resume the matching current active unwind record; absence is a VM fault.
    ActiveFinally,
}
#[derive(Clone, Copy, Debug)]
pub struct InstructionSpec {
    pub opcode: Opcode,
    pub mnemonic: &'static str,
    pub operands: [OperandKind; 3],
    pub allowed_flags: u16,
    pub flow: Flow,
    pub may_throw: bool,
    pub suspension: Suspension,
    /// Internal work may pause at a safe point, even in NoWait contexts.
    pub budget_resumable: bool,
    pub unwind: Unwind,
    pub semantic_rule: SemanticRule,
}
macro_rules! instructions {
    ($($name:ident = $number:literal => [$a:expr, $b:expr, $c:expr], $flow:ident, $throws:literal, $suspend:ident;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum Opcode { $($name = $number,)+ }
        pub static INSTRUCTION_SPECS: &[InstructionSpec] = &[$(
            InstructionSpec { opcode: Opcode::$name, mnemonic: stringify!($name),
                operands: [$a, $b, $c], allowed_flags: 0, flow: Flow::$flow,
                may_throw: $throws, suspension: Suspension::$suspend, budget_resumable: true,
                unwind: match Flow::$flow { Flow::Throw=>Unwind::Raise, Flow::Return|Flow::Leave=>Unwind::Exit, Flow::EndFinally=>Unwind::ResumeActive, _=>Unwind::None },
                semantic_rule: semantic_rule(Opcode::$name) },
        )+];
        impl Opcode { pub fn spec(self) -> &'static InstructionSpec { instruction_spec(self as u16).expect("declared opcode") } }
    };
}
use DescriptorKind as D;
use OperandKind::Pc;
use OperandKind::*;
instructions! {
    LoadConst = 0x0001 => [WriteReg, Const, Unused], Next, false, None;
    Move = 0x0002 => [WriteReg, ReadReg, Unused], Next, false, None;
    Clear = 0x0003 => [WriteReg, Unused, Unused], Next, false, None;
    BindInit = 0x0004 => [BindingInit, ReadReg, Unused], Next, true, None;
    BindRead = 0x0005 => [WriteReg, BindingRead, Unused], Next, true, None;
    BindWrite = 0x0006 => [BindingWrite, ReadReg, Unused], Next, true, None;
    CellNew = 0x0007 => [WriteReg, Binding, Unused], Next, true, None;
    CellRead = 0x0008 => [WriteReg, ReadReg, Unused], Next, true, None;
    CellWrite = 0x0009 => [ReadReg, ReadReg, Unused], Next, true, None;
    CaptureRead = 0x000a => [WriteReg, Capture, Unused], Next, true, None;
    GlobalRead = 0x000b => [WriteReg, Descriptor(D::Global), Unused], Next, true, None;
    GlobalInit = 0x000c => [ReadReg, Descriptor(D::Global), Unused], Next, true, None;
    GlobalWrite = 0x000d => [ReadReg, Descriptor(D::Global), Unused], Next, true, None;
    Add = 0x0010 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Sub = 0x0011 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Mul = 0x0012 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Div = 0x0013 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Rem = 0x0014 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    BitAnd = 0x0015 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    BitOr = 0x0016 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    BitXor = 0x0017 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Shl = 0x0018 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Shr = 0x0019 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Eq = 0x001a => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Ne = 0x001b => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Lt = 0x001c => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Le = 0x001d => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Gt = 0x001e => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Ge = 0x001f => [WriteReg, ReadReg, ReadReg], Next, true, None;
    Neg = 0x0020 => [WriteReg, ReadReg, Unused], Next, true, None;
    Pos = 0x0021 => [WriteReg, ReadReg, Unused], Next, true, None;
    Not = 0x0022 => [WriteReg, ReadReg, Unused], Next, true, None;
    BitNot = 0x0023 => [WriteReg, ReadReg, Unused], Next, true, None;
    Is = 0x0024 => [WriteReg, ReadReg, Type], Next, true, None;
    Jump = 0x0030 => [Pc, Unused, Unused], Jump, false, None;
    BranchBool = 0x0031 => [ReadReg, Pc, Pc], Branch, true, None;
    BranchNull = 0x0032 => [ReadReg, Pc, Pc], Branch, false, None;
    MakeClosure = 0x0040 => [WriteReg, Descriptor(D::Closure), Unused], Next, true, None;
    Call = 0x0041 => [WriteReg, Descriptor(D::Call), Unused], Next, true, Wait;
    Return = 0x0042 => [ReadReg, Unused, Unused], Return, true, None;
    ArgsNew = 0x0043 => [WriteReg, Unused, Unused], Next, true, None;
    ArgsPush = 0x0044 => [ReadReg, ReadReg, Unused], Next, true, None;
    ArgsNamed = 0x0045 => [ReadReg, ReadReg, Symbol], Next, true, None;
    ArgsSpread = 0x0046 => [ReadReg, ReadReg, Unused], Next, true, None;
    ArgsNamedSpread = 0x0047 => [ReadReg, ReadReg, Unused], Next, true, None;
    MakeTuple = 0x0050 => [WriteReg, Descriptor(D::Sequence), Unused], Next, true, None;
    MakeArray = 0x0051 => [WriteReg, Descriptor(D::Sequence), Unused], Next, true, None;
    MakeTable = 0x0052 => [WriteReg, Descriptor(D::Table), Unused], Next, true, None;
    IndexGet = 0x0053 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    IndexSet = 0x0054 => [ReadReg, ReadReg, ReadReg], Next, true, None;
    MemberGet = 0x0055 => [WriteReg, ReadReg, Symbol], Next, true, None;
    MemberSet = 0x0056 => [ReadReg, ReadReg, Symbol], Next, true, None;
    TableSpread = 0x0057 => [ReadReg, ReadReg, Unused], Next, true, None;
    ArraySpread = 0x0058 => [ReadReg, ReadReg, Unused], Next, true, None;
    MakeClass = 0x0059 => [WriteReg, Descriptor(D::Class), Unused], Next, true, None;
    SuperGet = 0x005a => [WriteReg, ReadReg, Descriptor(D::Super)], Next, true, None;
    PropertyRef = 0x005b => [WriteReg, ReadReg, Descriptor(D::Property)], Next, true, None;
    Match = 0x0060 => [WriteReg, ReadReg, Descriptor(D::Pattern)], Next, true, None;
    PatternRequire = 0x0061 => [ReadReg, Unused, Unused], Next, true, None;
    IterInit = 0x0062 => [WriteReg, ReadReg, Unused], Next, true, None;
    IterNext = 0x0063 => [WriteReg, ReadReg, Unused], Next, true, None;
    IterClose = 0x0064 => [ReadReg, Unused, Unused], Next, true, None;
    AsyncIterInit = 0x0065 => [WriteReg, ReadReg, Unused], Next, true, None;
    AsyncIterNext = 0x0066 => [WriteReg, ReadReg, Unused], Next, true, None;
    AsyncIterClose = 0x0067 => [WriteReg, ReadReg, Unused], Next, true, None;
    Throw = 0x0070 => [ReadReg, Unused, Unused], Throw, true, None;
    Rethrow = 0x0071 => [Unused, Unused, Unused], Throw, true, None;
    Leave = 0x0072 => [Pc, Unused, Unused], Leave, true, None;
    EndFinally = 0x0073 => [Region, Unused, Unused], EndFinally, true, None;
    Yield = 0x0080 => [WriteReg, ReadReg, Unused], Next, true, Yield;
    Await = 0x0081 => [WriteReg, ReadReg, Unused], Next, true, Wait;
    Spawn = 0x0082 => [WriteReg, Descriptor(D::Spawn), Unused], Next, true, None;
    Cancel = 0x0083 => [ReadReg, ReadReg, Unused], Next, true, None;
    ThreadNew = 0x0084 => [WriteReg, ReadReg, Unused], Next, true, None;
    ThreadResume = 0x0085 => [WriteReg, ReadReg, ReadReg], Next, true, None;
    ThreadSuspend = 0x0086 => [WriteReg, ReadReg, Unused], Next, true, Thread;
    ThreadClose = 0x0087 => [ReadReg, Unused, Unused], Next, true, None;
}
pub fn instruction_spec(opcode: u16) -> Option<&'static InstructionSpec> {
    INSTRUCTION_SPECS
        .binary_search_by_key(&opcode, |s| s.opcode as u16)
        .ok()
        .map(|i| &INSTRUCTION_SPECS[i])
}
const fn semantic_rule(op: Opcode) -> SemanticRule {
    match op {
        Opcode::BindInit | Opcode::GlobalInit => SemanticRule::BindingInit,
        Opcode::BindWrite | Opcode::CellWrite | Opcode::GlobalWrite => SemanticRule::BindingWrite,
        Opcode::CellNew => SemanticRule::CellContract,
        Opcode::ArgsPush | Opcode::ArgsNamed => SemanticRule::OrderedArguments,
        Opcode::ArgsSpread
        | Opcode::ArgsNamedSpread
        | Opcode::TableSpread
        | Opcode::ArraySpread => SemanticRule::SpreadSnapshot,
        Opcode::Match => SemanticRule::PatternCommit,
        Opcode::MemberGet => SemanticRule::MemberLookup,
        Opcode::EndFinally => SemanticRule::ActiveFinally,
        _ => SemanticRule::Ordinary,
    }
}

impl InstructionSpec {
    /// Shared register contract for verifier, lowering analyses and dispatcher.
    /// Windows are visited in order without allocating temporary register lists.
    pub fn visit_registers(
        &self,
        f: &FunctionDef,
        i: &Instruction,
        mut visit: impl FnMut(Reg, Access),
    ) -> Result<(), Error> {
        for (kind, operand) in self.operands.iter().zip(i.operands) {
            match kind {
                ReadReg => visit(Reg(operand), Access::Read),
                WriteReg => visit(Reg(operand), Access::Write),
                ReadWriteReg => visit(Reg(operand), Access::ReadWrite),
                BindingRead | BindingWrite | BindingInit => {
                    let b = f
                        .bindings
                        .get(operand as usize)
                        .ok_or_else(|| Error::new(ErrorKind::Index, "binding"))?;
                    let access = match (kind, b.storage) {
                        (BindingRead, _) | (_, BindingStorage::Cell(_)) => Access::Read,
                        _ => Access::Write,
                    };
                    visit(b.storage.reg(), access);
                }
                Descriptor(_) => {
                    let d = f
                        .descriptors
                        .get(operand as usize)
                        .ok_or_else(|| Error::new(ErrorKind::Index, "descriptor"))?;
                    d.visit_registers(f, &mut visit)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
impl CallDesc {
    fn visit_registers(&self, visit: &mut impl FnMut(Reg, Access)) {
        if let CallTarget::Register(reg) = self.target {
            visit(reg, Access::Read);
        }
        match self.arguments {
            Arguments::Window(w) => visit_window(w, visit),
            Arguments::Packed(reg) => visit(reg, Access::Read),
        }
    }
}
fn visit_window(w: RegWindow, visit: &mut impl FnMut(Reg, Access)) {
    // The verifier checks addition and bounds before invoking this accessor.
    for offset in 0..w.len {
        if let Some(r) = w.start.0.checked_add(offset) {
            visit(Reg(r), Access::Read);
        }
    }
}
impl super::Descriptor {
    fn visit_registers(
        &self,
        f: &FunctionDef,
        visit: &mut impl FnMut(Reg, Access),
    ) -> Result<(), Error> {
        match self {
            Self::Closure { captures, .. } => {
                for source in captures {
                    if let CaptureSource::Binding(b) = source {
                        let b = f
                            .bindings
                            .get(b.index())
                            .ok_or_else(|| Error::new(ErrorKind::Index, "capture binding"))?;
                        visit(b.storage.reg(), Access::Read);
                    }
                }
            }
            Self::Call(c) => c.visit_registers(visit),
            Self::Sequence(w) => visit_window(*w, visit),
            Self::Table(pairs) => {
                for pair in pairs {
                    for r in pair {
                        visit(*r, Access::Read);
                    }
                }
            }
            Self::Class(c) => {
                if let Some(r) = c.base {
                    visit(r, Access::Read);
                }
                for source in c
                    .fields
                    .iter()
                    .flat_map(|x| &x.captures)
                    .chain(c.methods.iter().flat_map(|x| &x.captures))
                {
                    if let CaptureSource::Binding(id) = source {
                        let b = f
                            .bindings
                            .get(id.index())
                            .ok_or_else(|| Error::new(ErrorKind::Index, "class capture binding"))?;
                        visit(b.storage.reg(), Access::Read);
                    }
                }
            }
            Self::Super { base, .. } => visit(*base, Access::Read),
            Self::Property { key, .. } => visit(*key, Access::Read),
            Self::Pattern(p) => {
                for node in &p.nodes {
                    if let PatternNode::Bind { output, .. } = node {
                        visit(*output, Access::ConditionalWrite);
                    }
                    if let PatternNode::Object { fields, .. } = node {
                        for source in fields.iter().flat_map(|f| &f.captures) {
                            if let CaptureSource::Binding(id) = source {
                                let binding = f.bindings.get(id.index()).ok_or_else(|| {
                                    Error::new(ErrorKind::Index, "pattern default capture")
                                })?;
                                visit(binding.storage.reg(), Access::Read);
                            }
                        }
                    }
                }
            }
            Self::Spawn { call, owner } => {
                visit(*owner, Access::Read);
                call.visit_registers(visit);
            }
            Self::Global { .. } => {}
        }
        Ok(())
    }
}

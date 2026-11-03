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
    /// Reading a binding/cell/global requires its independent initialized state.
    BindingRead,
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
        const OPCODE_SPEC_INDEX: [u8; 256] = {
            let opcodes = [$($number,)+];
            assert!(opcodes.len() <= u8::MAX as usize);
            let mut index = [u8::MAX; 256];
            let mut position = 0;
            while position < opcodes.len() {
                index[opcodes[position]] = position as u8;
                position += 1;
            }
            index
        };
        impl Opcode { pub fn spec(self) -> &'static InstructionSpec { instruction_spec(self as u16).expect("declared opcode") } }
    };
}
use DescriptorKind as D;
use OperandKind::Pc;
use OperandKind::*;
instructions! {
    LoadConst = 0x0001 => [WriteReg, Const, Unused], Next, true, None;
    Move = 0x0002 => [WriteReg, ReadReg, Unused], Next, false, None;
    Clear = 0x0003 => [WriteReg, Unused, Unused], Next, false, None;
    BindInit = 0x0004 => [BindingInit, ReadReg, Unused], Next, true, None;
    BindRead = 0x0005 => [WriteReg, BindingRead, Unused], Next, true, None;
    BindWrite = 0x0006 => [BindingWrite, ReadReg, Unused], Next, true, None;
    CellNew = 0x0007 => [Binding, Unused, Unused], Next, true, None;
    CellRead = 0x0008 => [WriteReg, ReadReg, Unused], Next, true, None;
    CellWrite = 0x0009 => [ReadReg, ReadReg, Unused], Next, true, None;
    CaptureRead = 0x000a => [WriteReg, Capture, Unused], Next, true, None;
    GlobalRead = 0x000b => [WriteReg, Descriptor(D::Global), Unused], Next, true, None;
    GlobalInit = 0x000c => [ReadReg, Descriptor(D::Global), Unused], Next, true, None;
    GlobalWrite = 0x000d => [ReadReg, Descriptor(D::Global), Unused], Next, true, None;
    Clear2 = 0x000e => [WriteReg, WriteReg, Unused], Next, false, None;
    Clear3 = 0x000f => [WriteReg, WriteReg, WriteReg], Next, false, None;
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
    TextNew = 0x0025 => [WriteReg, Unused, Unused], Next, true, None;
    TextPush = 0x0026 => [ReadReg, ReadReg, Unused], Next, true, None;
    TextFinish = 0x0027 => [WriteReg, ReadReg, Unused], Next, true, None;
    CheckType = 0x0028 => [ReadReg, Type, Unused], Next, true, None;
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
    CallNoWait = 0x0048 => [WriteReg, Descriptor(D::Call), Unused], Next, true, None;
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
    ChildrenPush = 0x005c => [ReadReg, ReadReg, Unused], Next, true, None;
    MakeRange = 0x005d => [WriteReg, ReadReg, ReadReg], Next, true, None;
    MakeRangeInclusive = 0x005e => [WriteReg, ReadReg, ReadReg], Next, true, None;
    ArrayPush = 0x005f => [ReadReg, ReadReg, Unused], Next, true, None;
    Match = 0x0060 => [WriteReg, ReadReg, Descriptor(D::Pattern)], Next, true, None;
    PatternRequire = 0x0061 => [ReadReg, Unused, Unused], Next, true, None;
    IterInit = 0x0062 => [WriteReg, ReadReg, Unused], Next, true, None;
    IterNext = 0x0063 => [WriteReg, ReadReg, Unused], Next, true, None;
    IterClose = 0x0064 => [ReadReg, Unused, Unused], Next, true, None;
    AsyncIterInit = 0x0065 => [WriteReg, ReadReg, Unused], Next, true, None;
    AsyncIterNext = 0x0066 => [WriteReg, ReadReg, Unused], Next, true, None;
    AsyncIterClose = 0x0067 => [WriteReg, ReadReg, Unused], Next, true, None;
    IterAdvance = 0x0068 => [WriteReg, ReadReg, WriteReg], Next, true, None;
    MatchFail = 0x0069 => [Unused, Unused, Unused], Throw, true, None;
    SuperRef = 0x006a => [WriteReg, Unused, Unused], Next, true, None;
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
/// Finds a specification through the frozen opcode domain in constant time.
/// Unknown values and unused opcode slots return `None`.
#[inline]
pub fn instruction_spec(opcode: u16) -> Option<&'static InstructionSpec> {
    INSTRUCTION_SPECS.get(*OPCODE_SPEC_INDEX.get(opcode as usize)? as usize)
}
#[cfg(feature = "execution-counts")]
pub(crate) fn instruction_spec_index(opcode: Opcode) -> usize {
    OPCODE_SPEC_INDEX[opcode as usize] as usize
}
const fn semantic_rule(op: Opcode) -> SemanticRule {
    match op {
        Opcode::BindRead | Opcode::CellRead | Opcode::GlobalRead => SemanticRule::BindingRead,
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
    /// Unmetered adapter for already trusted analyses. Verification uses the
    /// budgeted form, which charges known lengths before traversing a window.
    pub fn visit_registers(
        &self,
        f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
        i: &Instruction,
        visit: impl FnMut(Reg, Access),
    ) -> Result<(), Error> {
        self.visit_registers_budgeted(f, i, |_| Ok(()), visit)
    }
    pub fn visit_registers_budgeted(
        &self,
        f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
        i: &Instruction,
        mut charge: impl FnMut(u64) -> Result<(), Error>,
        mut visit: impl FnMut(Reg, Access),
    ) -> Result<(), Error> {
        let mut visitor = RegisterVisitor {
            charge: &mut charge,
            visit: &mut visit,
        };
        visitor.work(3)?;
        for (kind, operand) in self.operands.iter().zip(i.operands) {
            match kind {
                Binding => {
                    let binding = f
                        .bindings
                        .get(operand as usize)
                        .ok_or_else(|| Error::new(ErrorKind::Index, "cell binding contract"))?;
                    visitor.emit(binding.storage.reg(), Access::Write)?;
                }
                ReadReg => visitor.emit(Reg(operand), Access::Read)?,
                WriteReg => visitor.emit(Reg(operand), Access::Write)?,
                ReadWriteReg => visitor.emit(Reg(operand), Access::ReadWrite)?,
                BindingRead | BindingWrite | BindingInit => {
                    let binding = f
                        .bindings
                        .get(operand as usize)
                        .ok_or_else(|| Error::new(ErrorKind::Index, "binding"))?;
                    let access = match (kind, binding.storage) {
                        (BindingRead, _) | (_, BindingStorage::Cell(_)) => Access::Read,
                        _ => Access::Write,
                    };
                    visitor.emit(binding.storage.reg(), access)?;
                }
                Descriptor(_) => f
                    .descriptors
                    .get(operand as usize)
                    .ok_or_else(|| Error::new(ErrorKind::Index, "descriptor"))?
                    .visit_registers(f, &mut visitor)?,
                _ => {}
            }
        }
        Ok(())
    }
}
struct RegisterVisitor<'a> {
    charge: &'a mut dyn FnMut(u64) -> Result<(), Error>,
    visit: &'a mut dyn FnMut(Reg, Access),
}
impl RegisterVisitor<'_> {
    fn work(&mut self, units: u64) -> Result<(), Error> {
        (self.charge)(units)
    }
    fn emit(&mut self, reg: Reg, access: Access) -> Result<(), Error> {
        self.work(1)?;
        (self.visit)(reg, access);
        Ok(())
    }
    fn window(&mut self, window: RegWindow) -> Result<(), Error> {
        let end = window
            .start
            .0
            .checked_add(window.len)
            .ok_or_else(|| Error::new(ErrorKind::Window, "register window overflow"))?;
        self.work(window.len as u64)?;
        for reg in window.start.0..end {
            (self.visit)(Reg(reg), Access::Read);
        }
        Ok(())
    }
    fn captures(
        &mut self,
        f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
        captures: &[CaptureSource],
    ) -> Result<(), Error> {
        self.work(captures.len() as u64)?;
        for source in captures {
            if let CaptureSource::Binding(id) = source {
                let binding = f
                    .bindings
                    .get(id.index())
                    .ok_or_else(|| Error::new(ErrorKind::Index, "capture binding"))?;
                self.emit(binding.storage.reg(), Access::Read)?;
            }
        }
        Ok(())
    }
}
impl CallDesc {
    fn visit_registers(&self, visitor: &mut RegisterVisitor<'_>) -> Result<(), Error> {
        if let CallTarget::Register(reg) = self.target {
            visitor.emit(reg, Access::Read)?;
        }
        match self.arguments {
            Arguments::Window(window) => visitor.window(window),
            Arguments::Packed(reg) => visitor.emit(reg, Access::Read),
        }
    }
}
impl super::Descriptor {
    fn visit_registers(
        &self,
        f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
        visitor: &mut RegisterVisitor<'_>,
    ) -> Result<(), Error> {
        match self {
            Self::Closure { captures, .. } => visitor.captures(f, captures)?,
            Self::Call(call) => call.visit_registers(visitor)?,
            Self::Sequence(window) => visitor.window(*window)?,
            Self::Table(pairs) => {
                visitor.work(pairs.len() as u64)?;
                for pair in pairs {
                    for reg in pair {
                        visitor.emit(*reg, Access::Read)?;
                    }
                }
            }
            Self::TableSources(sources) => {
                visitor.work(sources.len() as u64)?;
                for source in sources {
                    match source {
                        TableSource::Pair(pair) => {
                            visitor.emit(pair[0], Access::Read)?;
                            visitor.emit(pair[1], Access::Read)?;
                        }
                        TableSource::Spread(reg) => visitor.emit(*reg, Access::Read)?,
                    }
                }
            }
            Self::Class(class) => {
                if let Some(base) = class.base {
                    visitor.emit(base, Access::Read)?;
                }
                visitor.work((class.fields.len() + class.methods.len()) as u64)?;
                for field in &class.fields {
                    visitor.captures(f, &field.captures)?;
                }
                for method in &class.methods {
                    visitor.captures(f, &method.captures)?;
                }
            }
            Self::Super { base, .. } => visitor.emit(*base, Access::Read)?,
            Self::Property { key, .. } => visitor.emit(*key, Access::Read)?,
            Self::Pattern(pattern) => {
                visitor.work(pattern.nodes.len() as u64)?;
                for node in &pattern.nodes {
                    if let PatternNode::Bind { output, .. } = node {
                        visitor.emit(*output, Access::ConditionalWrite)?;
                    }
                    if let PatternNode::Object { fields, .. } = node {
                        visitor.work(fields.len() as u64)?;
                        for field in fields {
                            visitor.captures(f, &field.captures)?;
                        }
                    }
                }
            }
            Self::Spawn { call, owner } => {
                visitor.emit(*owner, Access::Read)?;
                call.visit_registers(visitor)?;
            }
            Self::Global { .. } => {}
        }
        Ok(())
    }
}

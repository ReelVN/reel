//! Small execution cursor shared by bounded register and Body contexts.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Cursor {
    pub(super) function: FunctionId,
    pub(super) pc: Pc,
    pub(super) instruction_end: Pc,
    pub(super) member_symbol: Option<SymbolId>,
    pub(super) instruction: Option<Instruction>,
    pub(super) instruction_pc: Pc,
}

impl Cursor {
    pub(super) fn capture(frame: &ScriptFrame) -> Self {
        Self {
            function: frame.code.function,
            pc: frame.pc,
            instruction_end: frame.instruction_end,
            member_symbol: frame.member_symbol,
            instruction: frame.instruction,
            instruction_pc: frame.instruction_pc,
        }
    }
    pub(super) fn publish(self, frame: &mut ScriptFrame) {
        frame.pc = self.pc;
        frame.instruction_end = self.instruction_end;
        frame.member_symbol = self.member_symbol;
        frame.instruction = self.instruction;
        frame.instruction_pc = self.instruction_pc;
    }
    pub(super) fn advance(&mut self, next: Pc) {
        self.pc = next;
        self.instruction = None;
        self.member_symbol = None;
    }
}

/// Outcome of a bounded leaf step; cold faults publish after all views end.
pub(super) enum Exit {
    Done,
    General,
    Budget,
    Fault(FaultKind),
}

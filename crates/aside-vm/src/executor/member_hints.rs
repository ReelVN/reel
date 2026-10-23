//! Bounded, non-owning positions for named member lookups. A hint never owns a
//! value or skips current-key validation; table versions and property semantics
//! remain in the ordinary lookup/commit path. Snapshots restore this cold.
use super::*;

const SLOTS: usize = 256;
pub(super) const MAX_KEY_BYTES: usize = 64;

#[derive(Clone, Copy)]
struct Hint {
    function: u32,
    pc: u32,
    entry: u32,
}

pub(super) struct MemberHints([Hint; SLOTS]);

impl Default for MemberHints {
    fn default() -> Self {
        Self(
            [Hint {
                function: u32::MAX,
                pc: 0,
                entry: 0,
            }; SLOTS],
        )
    }
}

impl MemberHints {
    fn slot(function: FunctionId, pc: Pc) -> usize {
        (function.0.wrapping_mul(0x9e37_79b9) ^ pc.0.wrapping_mul(0x85eb_ca6b)) as usize
            & (SLOTS - 1)
    }

    pub(super) fn get(&self, function: FunctionId, pc: Pc) -> Option<u32> {
        let hint = self.0[Self::slot(function, pc)];
        (hint.function == function.0 && hint.pc == pc.0).then_some(hint.entry)
    }

    pub(super) fn remember(&mut self, function: FunctionId, pc: Pc, entry: u32) {
        self.0[Self::slot(function, pc)] = Hint {
            function: function.0,
            pc: pc.0,
            entry,
        };
    }
}

impl Vm {
    pub(super) fn member_hint_matches(
        &self,
        table: Value,
        entry: u32,
        symbol: SymbolId,
    ) -> Result<bool, VmError> {
        let name = self.program.package().symbols[symbol.index()].as_bytes();
        let (key, _) = self.heap.table_entry(table, entry)?;
        if key.tag() != ValueTag::Object {
            return Ok(false);
        }
        let info = self.heap.info(key)?;
        if info.kind != ObjectKind::String || info.len as usize != name.len() {
            return Ok(false);
        }
        let mut bytes = [0; MAX_KEY_BYTES];
        self.heap.read_bytes(key, 0, &mut bytes[..name.len()])?;
        Ok(&bytes[..name.len()] == name)
    }
}

// This fixed storage is included by Vm's existing control-size reservation.
const _: () = assert!(std::mem::size_of::<MemberHints>() == 3_072);

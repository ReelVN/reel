//! Bounded, non-owning positions for named member lookups. A hint never owns a
//! value. A full object generation and structural version can reuse a proven
//! key position; other receivers validate its current key. Snapshots restore cold.
//! Tables and classes use separate fixed stores: class reads cannot evict table
//! positions whose hit/miss paths have different budgeted work. Classes guard
//! the member array's content version and recheck symbol/static keys.
use super::*;
use std::cell::Cell;

const SLOTS: usize = 256;
pub(super) const CLASS_SLOTS: usize = 64;
pub(super) const MAX_KEY_BYTES: usize = 64;

#[derive(Clone, Copy)]
struct Hint {
    function: u32,
    pc: u32,
    entry: u32,
    table: u64,
    version: u64,
}

pub(super) struct MemberHints<const N: usize = SLOTS> {
    entries: [Cell<Hint>; N],
    #[cfg(test)]
    pub(super) identities: bool,
}

impl<const N: usize> Default for MemberHints<N> {
    fn default() -> Self {
        assert!(N.is_power_of_two());
        Self {
            entries: std::array::from_fn(|_| {
                Cell::new(Hint {
                    function: u32::MAX,
                    pc: 0,
                    entry: 0,
                    table: 0,
                    version: 0,
                })
            }),
            #[cfg(test)]
            identities: true,
        }
    }
}

impl<const N: usize> MemberHints<N> {
    fn slot(function: FunctionId, pc: Pc) -> usize {
        (function.0.wrapping_mul(0x9e37_79b9) ^ pc.0.wrapping_mul(0x85eb_ca6b)) as usize & (N - 1)
    }

    pub(super) fn get(
        &self,
        function: FunctionId,
        pc: Pc,
        table: Value,
        version: u64,
    ) -> Option<(u32, bool)> {
        let hint = self.entries[Self::slot(function, pc)].get();
        let trusted = hint.table == table.object_parts().unwrap().1 && hint.version == version;
        #[cfg(test)]
        let trusted = trusted && self.identities;
        (hint.function == function.0 && hint.pc == pc.0).then_some((hint.entry, trusted))
    }

    pub(super) fn remember(
        &self,
        function: FunctionId,
        pc: Pc,
        entry: u32,
        table: Value,
        version: u64,
    ) {
        self.entries[Self::slot(function, pc)].set(Hint {
            function: function.0,
            pc: pc.0,
            entry,
            table: table.object_parts().unwrap().1,
            version,
        });
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
const _: () = assert!(std::mem::size_of::<[Cell<Hint>; SLOTS]>() == 8_192);
const _: () = assert!(std::mem::size_of::<[Cell<Hint>; CLASS_SLOTS]>() == 2_048);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn class_positions_cannot_evict_the_original_table_slot_domain() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = heap.alloc_table().unwrap();
        let members = heap.alloc_array(&[]).unwrap();
        let tables = MemberHints::<SLOTS>::default();
        let classes = MemberHints::<CLASS_SLOTS>::default();
        for slot in 0..SLOTS {
            let pc = Pc(slot as u32);
            let expected_slot = pc.0.wrapping_mul(0x85eb_ca6b) as usize & 255;
            assert_eq!(MemberHints::<SLOTS>::slot(FunctionId(0), pc), expected_slot);
            tables.remember(FunctionId(0), pc, slot as u32, table, 7);
        }
        // Each of these sites collides with an occupied table slot under the
        // old shared store. Replacing every class position leaves tables intact.
        for pc in 0..1024 {
            classes.remember(FunctionId(0), Pc(pc), pc, members, 11);
        }
        for slot in 0..SLOTS {
            assert_eq!(
                tables.get(FunctionId(0), Pc(slot as u32), table, 7),
                Some((slot as u32, true))
            );
        }
        assert_eq!(std::mem::size_of_val(&tables.entries), 8192);
        assert_eq!(std::mem::size_of_val(&classes.entries), 2048);
    }

    #[test]
    fn identity_proofs_include_generation_and_structure_without_rooting_receivers() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = heap.alloc_table().unwrap();
        let key = heap.alloc_string("value").unwrap();
        heap.table_store(table, 0, key, Value::int(1)).unwrap();
        let version = heap.info(table).unwrap().structure_version;
        let hints = MemberHints::<SLOTS>::default();
        hints.remember(FunctionId(0), Pc(1), 0, table, version);
        assert_eq!(
            hints.get(FunctionId(0), Pc(1), table, version),
            Some((0, true))
        );
        heap.table_store(table, 0, key, Value::int(2)).unwrap();
        assert_eq!(
            hints.get(
                FunctionId(0),
                Pc(1),
                table,
                heap.info(table).unwrap().structure_version
            ),
            Some((0, true))
        );
        let second = heap.alloc_string("second").unwrap();
        heap.table_store(table, 1, second, Value::int(3)).unwrap();
        assert_eq!(
            hints.get(
                FunctionId(0),
                Pc(1),
                table,
                heap.info(table).unwrap().structure_version
            ),
            Some((0, false))
        );
        let (_, bits) = table.object_parts().unwrap();
        let next_generation = Value::object(heap.id(), bits ^ (1u64 << 32));
        assert_eq!(
            hints.get(FunctionId(0), Pc(1), next_generation, version),
            Some((0, false))
        );
        heap.begin_collection().unwrap();
        while heap.phase() != crate::GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
        assert!(heap.info(table).is_err());
    }
}

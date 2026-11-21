//! Non-owning positions for repeated string and integer queries. Full object
//! generations and table structure guard hits. Restored VMs start cold.
use super::*;

const SLOTS: usize = 128;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum KeyKind {
    #[default]
    Text,
    Integer,
}

#[derive(Clone, Copy, Default)]
struct Entry {
    table: u64,
    key: u64,
    version: u64,
    position: u32,
    kind: KeyKind,
}
pub(in crate::executor) struct Queries([Entry; SLOTS]);
impl Default for Queries {
    fn default() -> Self {
        Self([Entry::default(); SLOTS])
    }
}
impl Queries {
    fn slot(table: u64, key: u64) -> usize {
        let hash = table.wrapping_mul(0x9e37_79b9) ^ key.wrapping_mul(0x85eb_ca6b);
        ((hash ^ (hash >> 32)) as usize) & (SLOTS - 1)
    }
    fn key(value: Value) -> Option<(KeyKind, u64)> {
        if let Some(value) = value.as_int() {
            Some((KeyKind::Integer, value as u64))
        } else {
            Some((KeyKind::Text, value.object_parts()?.1))
        }
    }
    pub(super) fn get(&self, table: Value, key: Value, version: u64) -> Option<u32> {
        let table = table.object_parts()?.1;
        let (kind, key) = Self::key(key)?;
        let entry = self.0[Self::slot(table, key)];
        (entry.table == table && entry.key == key && entry.kind == kind && entry.version == version)
            .then_some(entry.position)
    }
    pub(super) fn remember(&mut self, table: Value, key: Value, version: u64, position: u32) {
        let table = table.object_parts().unwrap().1;
        let (kind, key) = Self::key(key).unwrap();
        self.0[Self::slot(table, key)] = Entry {
            table,
            key,
            version,
            position,
            kind,
        };
    }
}

impl Vm {
    pub(super) fn query_key(
        &self,
        frame: &ScriptFrame,
        len: u32,
    ) -> Result<Option<Value>, VmError> {
        if len < 16
            || !matches!(
                frame.table_work.unwrap().action,
                TableAction::Get(_)
                    | TableAction::PatternRead { .. }
                    | TableAction::GetDefault { .. }
                    | TableAction::HasOwn(_)
            )
        {
            return Ok(None);
        }
        let key = self.value(frame.env, 2)?;
        Ok((key.tag() == ValueTag::Int || self.is_text(key, ObjectKind::String)).then_some(key))
    }
}

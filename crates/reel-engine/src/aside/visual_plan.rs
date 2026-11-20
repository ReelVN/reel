//! Build-time image candidates indexed by the suspended call's bytecode PC.
use super::visual_prediction::{
    Catalogs, IMAGE_LIMIT, RESUME_LOOKUP_LIMIT, compile_site, image_path,
};
use arrayvec::ArrayVec;
use aside_vm::{
    bytecode::{self as bc, Opcode},
    executor::HostCallSite,
};
use std::collections::HashMap;

const HEADER_BYTES: usize = 4;
const RECORD_BYTES: usize = 16;
const ID_BYTES: usize = 4;
const BUILD_WORK: usize = 8_000_000;
const CATALOG_BUILD_WORK: usize = BUILD_WORK / 4;

#[derive(Clone, Copy)]
struct Record {
    site: (u32, u32),
    start: u32,
    count: u32,
}

/// Bounded hints for a particular verified bytecode program. A hint can
/// select a resource to load but cannot change what the script executes.
#[derive(Debug)]
pub struct VisualPlan {
    bytes: Box<[u8]>,
    ids_start: usize,
}

impl VisualPlan {
    /// Analyze the final code before packaging, while original text is bound.
    /// Each recorded continuation is limited to a small forward walk. Equal
    /// image groups share an ID range in the output.
    pub fn compile(program: &bc::VerifiedProgram, byte_limit: usize) -> Option<Self> {
        let mut records = Vec::new();
        let mut ids = Vec::new();
        let mut groups = HashMap::<ArrayVec<u32, IMAGE_LIMIT>, u32>::new();
        let mut catalog_budget = CATALOG_BUILD_WORK;
        let catalogs = Catalogs::compile(program, &mut catalog_budget);
        let mut budget = BUILD_WORK - CATALOG_BUILD_WORK + catalog_budget;
        'functions: for (function_index, function) in program.package().functions.iter().enumerate()
        {
            if function.kind != bc::FunctionKind::Plain
                || function.context != bc::Context::Ordinary
                || !function.regions.is_empty()
            {
                continue;
            }
            for (pc, instruction) in function.code.indexed_iter().flatten() {
                let Some(remaining) = budget.checked_sub(1) else {
                    break 'functions;
                };
                budget = remaining;
                if !is_resume_call(instruction) {
                    continue;
                }
                let Some(next) = function.code.next_pc(pc).ok().flatten() else {
                    continue;
                };
                let Some(paths) = compile_site(program, &catalogs, function, next, &mut budget)
                else {
                    continue;
                };
                let group: ArrayVec<_, IMAGE_LIMIT> =
                    paths.into_iter().map(|path| path.constant.0).collect();
                let count = group.len();
                let existing = groups.get(&group).copied();
                let new_ids = if existing.is_none() { count } else { 0 };
                let size = HEADER_BYTES
                    + (records.len() + 1) * RECORD_BYTES
                    + (ids.len() + new_ids) * ID_BYTES;
                if size > byte_limit {
                    break 'functions;
                }
                let start = existing.unwrap_or_else(|| {
                    let start = ids.len() as u32;
                    ids.extend(group.iter().copied());
                    groups.insert(group, start);
                    start
                });
                records.push(Record {
                    site: (function_index as u32, pc as u32),
                    start,
                    count: count as u32,
                });
            }
        }
        if ids.is_empty() {
            return None;
        }
        let ids_start = HEADER_BYTES + records.len() * RECORD_BYTES;
        let mut bytes = Vec::with_capacity(ids_start + ids.len() * ID_BYTES);
        append_id(&mut bytes, records.len() as u32);
        for record in records {
            for word in [record.site.0, record.site.1, record.start, record.count] {
                append_id(&mut bytes, word);
            }
        }
        for id in ids {
            append_id(&mut bytes, id);
        }
        Some(Self {
            bytes: bytes.into_boxed_slice(),
            ids_start,
        })
    }

    /// The compact representation stored in the code package.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Validate ordered call positions and references before making the table
    /// available to the player. Retain the compact bytes for direct lookup.
    pub fn decode(bytes: Box<[u8]>, program: &bc::VerifiedProgram) -> Option<Self> {
        let count = read_id(bytes.get(..HEADER_BYTES)?) as usize;
        let ids_start = count.checked_mul(RECORD_BYTES)?.checked_add(HEADER_BYTES)?;
        let ids = bytes.get(ids_start..)?;
        if count == 0 || !ids.len().is_multiple_of(ID_BYTES) {
            return None;
        }
        for id in ids.as_chunks::<ID_BYTES>().0 {
            image_path(program, bc::ConstId(u32::from_le_bytes(*id)))?;
        }
        let plan = Self { bytes, ids_start };
        let mut previous = None;
        for bytes in plan.records() {
            let record = read_record(bytes);
            let (function, pc) = record.site;
            let function = program.package().functions.get(function as usize)?;
            if previous.is_some_and(|previous| record.site <= previous)
                || !is_resume_call(function.code.instruction(pc as usize).ok()??)
                || record.count as usize > IMAGE_LIMIT
                || record.start.checked_add(record.count)? as usize > plan.id_count()
            {
                return None;
            }
            previous = Some(record.site);
        }
        Some(plan)
    }

    pub(super) fn next_images<'p>(
        &self,
        program: &'p bc::VerifiedProgram,
        sites: impl Iterator<Item = HostCallSite>,
    ) -> Option<ArrayVec<&'p str, IMAGE_LIMIT>> {
        for site in sites.take(RESUME_LOOKUP_LIMIT) {
            let key = (site.code.function.0, site.pc.0);
            let records = self.records();
            let index = records
                .binary_search_by_key(&key, |bytes| (read_id(&bytes[..4]), read_id(&bytes[4..8])))
                .ok()?;
            let record = read_record(&records[index]);
            if record.count != 0 {
                return (record.start..record.start + record.count)
                    .map(|id| image_path(program, self.image_id(id as usize)))
                    .collect();
            }
        }
        None
    }

    fn records(&self) -> &[[u8; RECORD_BYTES]] {
        self.bytes[HEADER_BYTES..self.ids_start].as_chunks().0
    }

    fn id_count(&self) -> usize {
        (self.bytes.len() - self.ids_start) / ID_BYTES
    }

    fn image_id(&self, index: usize) -> bc::ConstId {
        let offset = self.ids_start + index * ID_BYTES;
        bc::ConstId(read_id(&self.bytes[offset..][..ID_BYTES]))
    }
}

fn read_record(bytes: &[u8; RECORD_BYTES]) -> Record {
    let [function, pc, start, count] =
        std::array::from_fn(|i| read_id(&bytes[i * ID_BYTES..][..ID_BYTES]));
    Record {
        site: (function, pc),
        start,
        count,
    }
}

fn is_resume_call(instruction: bc::Instruction) -> bool {
    matches!(
        bc::instruction_spec(instruction.opcode).map(|spec| spec.opcode),
        Some(Opcode::Call | Opcode::CallNoWait | Opcode::Await)
    )
}

fn read_id(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn append_id(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

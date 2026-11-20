//! Build-time image candidates for suspended calls and page factory entry.
use super::visual_prediction::{
    Catalogs, Classes, IMAGE_LIMIT, PagePaths, RESUME_LOOKUP_LIMIT, compile_page, compile_site,
    image_path,
};
use arrayvec::ArrayVec;
use aside_vm::{
    bytecode::{self as bc, Opcode},
    executor::HostCallSite,
};
use std::collections::{BTreeSet, HashMap};

const HEADER_BYTES: usize = 4;
const RECORD_BYTES: usize = 16;
const ID_BYTES: usize = 4;
const BUILD_WORK: usize = 8_000_000;
const METADATA_BUILD_WORK: usize = BUILD_WORK / 4;
/// A word offset cannot reach this value in verified bytecode.
const PAGE_ENTRY: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Record {
    site: (u32, u32),
    start: u32,
    count: u32,
}

#[derive(Default)]
struct PlanBuilder {
    records: Vec<Record>,
    ids: Vec<u32>,
    groups: HashMap<ArrayVec<u32, IMAGE_LIMIT>, u32>,
}

impl PlanBuilder {
    fn push(
        &mut self,
        site: (u32, u32),
        group: ArrayVec<u32, IMAGE_LIMIT>,
        byte_limit: usize,
    ) -> bool {
        let count = group.len();
        let existing = self.groups.get(&group).copied();
        let new_ids = if existing.is_none() { count } else { 0 };
        let size = HEADER_BYTES
            + (self.records.len() + 1) * RECORD_BYTES
            + (self.ids.len() + new_ids) * ID_BYTES;
        if size > byte_limit {
            return false;
        }
        let start = existing.unwrap_or_else(|| {
            let start = self.ids.len() as u32;
            self.ids.extend(group.iter().copied());
            self.groups.insert(group, start);
            start
        });
        self.records.push(Record {
            site,
            start,
            count: count as u32,
        });
        true
    }

    fn finish(self) -> Option<VisualPlan> {
        if self.ids.is_empty() {
            return None;
        }
        let ids_start = HEADER_BYTES + self.records.len() * RECORD_BYTES;
        let mut bytes = Vec::with_capacity(ids_start + self.ids.len() * ID_BYTES);
        append_id(&mut bytes, self.records.len() as u32);
        for record in self.records {
            for word in [record.site.0, record.site.1, record.start, record.count] {
                append_id(&mut bytes, word);
            }
        }
        for id in self.ids {
            append_id(&mut bytes, id);
        }
        Some(VisualPlan {
            bytes: bytes.into_boxed_slice(),
            ids_start,
        })
    }
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
        let mut plan = PlanBuilder::default();
        let mut metadata_budget = METADATA_BUILD_WORK;
        let catalogs = Catalogs::compile(program, &mut metadata_budget);
        let classes = Classes::compile(program, &mut metadata_budget);
        let mut budget = BUILD_WORK - METADATA_BUILD_WORK;
        'functions: for (function_index, function) in program.package().functions.iter().enumerate()
        {
            if function.kind != bc::FunctionKind::Plain
                || function.context != bc::Context::Ordinary
                || !function.regions.is_empty()
            {
                continue;
            }
            let mut parameters = None;
            for (pc, instruction) in function.code.indexed_iter().flatten() {
                let Some(remaining) = budget.checked_sub(1) else {
                    break 'functions;
                };
                budget = remaining;
                if !may_resume_here(program, function, instruction) {
                    continue;
                }
                let Some(next) = function.code.next_pc(pc).ok().flatten() else {
                    continue;
                };
                let parameters = parameters.get_or_insert_with(|| {
                    classes.stable_parameters(function, &mut metadata_budget)
                });
                let Some(paths) = compile_site(
                    program,
                    &catalogs,
                    &classes,
                    function,
                    next,
                    parameters,
                    &mut budget,
                ) else {
                    continue;
                };
                if !plan.push(
                    (function_index as u32, pc as u32),
                    paths.into_iter().map(|path| path.constant.0).collect(),
                    byte_limit,
                ) {
                    break 'functions;
                }
            }
        }
        let continuations = plan.records.len();
        let factories = page_factories(program, &mut budget);
        let page_paths = if factories.is_empty() {
            PagePaths::new()
        } else {
            page_path_constants(program, &mut budget)
        };
        for factory in factories {
            let function = &program.package().functions[factory.index()];
            let Some(paths) = compile_page(
                program,
                &catalogs,
                &classes,
                function,
                &page_paths,
                &mut budget,
            ) else {
                continue;
            };
            if !paths.is_empty()
                && !plan.push(
                    (factory.0, PAGE_ENTRY),
                    paths.into_iter().map(|path| path.constant.0).collect(),
                    byte_limit,
                )
            {
                break;
            }
        }
        if plan.records.len() > continuations {
            plan.records.sort_unstable_by_key(|record| record.site);
        }
        plan.finish()
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
            let valid_site = if pc == PAGE_ENTRY {
                record.count != 0 && is_page_factory(program, function)
            } else {
                is_resume_call(function.code.instruction(pc as usize).ok()??)
            };
            if previous.is_some_and(|previous| record.site <= previous)
                || !valid_site
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
            let record = self.record((site.code.function.0, site.pc.0))?;
            if record.count != 0 {
                return self.paths(program, record);
            }
        }
        None
    }

    pub(super) fn page_images<'p>(
        &self,
        program: &'p bc::VerifiedProgram,
        factory: bc::FunctionId,
    ) -> Option<ArrayVec<&'p str, IMAGE_LIMIT>> {
        self.paths(program, self.record((factory.0, PAGE_ENTRY))?)
    }

    fn record(&self, key: (u32, u32)) -> Option<Record> {
        let records = self.records();
        let index = records
            .binary_search_by_key(&key, |bytes| (read_id(&bytes[..4]), read_id(&bytes[4..8])))
            .ok()?;
        Some(read_record(&records[index]))
    }

    fn paths<'p>(
        &self,
        program: &'p bc::VerifiedProgram,
        record: Record,
    ) -> Option<ArrayVec<&'p str, IMAGE_LIMIT>> {
        (record.start..record.start + record.count)
            .map(|id| image_path(program, self.image_id(id as usize)))
            .collect()
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

fn is_page_factory(program: &bc::VerifiedProgram, function: &bc::RuntimeFunction) -> bool {
    let signature = &program.package().signatures[function.signature.index()];
    function.kind == bc::FunctionKind::Plain
        && function.regions.is_empty()
        && signature.parameters.is_empty()
        && signature.rest.is_none()
}

fn page_path_constants<'p>(program: &'p bc::VerifiedProgram, budget: &mut usize) -> PagePaths<'p> {
    let mut paths = PagePaths::new();
    for index in 0..program.package().constants.len() {
        let id = bc::ConstId(index as u32);
        let path = image_path(program, id);
        let Some(remaining) = budget.checked_sub(1 + path.map_or(0, |s| s.len().div_ceil(16)))
        else {
            break;
        };
        *budget = remaining;
        if let Some(path) = path {
            paths.entry(path).or_insert(id);
        }
    }
    paths
}

/// A page stack receives factory closures from ordinary script. Search only
/// functions referring to the constructor, and publish hints solely by the
/// ID of the actual factory the player later finds on a stack.
fn page_factories(program: &bc::VerifiedProgram, budget: &mut usize) -> BTreeSet<bc::FunctionId> {
    let package = program.package();
    let mut factories = BTreeSet::new();
    let Some(index) = package
        .natives
        .iter()
        .position(|native| package.symbols[native.symbol.index()] == "reel::page_stack")
    else {
        return factories;
    };
    let page_stack = bc::NativeId(index as u32);
    for function in &package.functions {
        let Some(remaining) = budget.checked_sub(function.descriptors.len().max(1)) else {
            break;
        };
        *budget = remaining;
        if !function.descriptors.iter().any(|descriptor| {
            let bc::Descriptor::Call(call) = descriptor else {
                return false;
            };
            match call.target {
                bc::CallTarget::Native(id) => id == page_stack,
                bc::CallTarget::Function(id) => {
                    package.functions[id.index()].kind == bc::FunctionKind::Native(page_stack)
                }
                bc::CallTarget::Register(_) => false,
            }
        }) {
            continue;
        }
        for descriptor in &function.descriptors {
            let target = match *descriptor {
                bc::Descriptor::Closure { function, .. } => Some(function),
                bc::Descriptor::Global { module, global } => package.modules[module.index()]
                    .globals
                    .get(global.index())
                    .filter(|global| !global.mutable)
                    .and_then(|global| global.linked_function),
                _ => None,
            };
            let Some(factory) = target else {
                continue;
            };
            if is_page_factory(program, &package.functions[factory.index()]) {
                factories.insert(factory);
            }
        }
    }
    factories
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

fn may_resume_here(
    program: &bc::VerifiedProgram,
    function: &bc::RuntimeFunction,
    instruction: bc::Instruction,
) -> bool {
    match bc::instruction_spec(instruction.opcode).map(|spec| spec.opcode) {
        Some(Opcode::Await) => true,
        Some(Opcode::Call) => {
            let bc::Descriptor::Call(call) =
                &function.descriptors[instruction.operands[1] as usize]
            else {
                return false;
            };
            let package = program.package();
            let signature = match call.target {
                bc::CallTarget::Native(id) => package.natives[id.index()].signature,
                bc::CallTarget::Function(id) => package.functions[id.index()].signature,
                bc::CallTarget::Register(_) => return true,
            };
            package.signatures[signature.index()].effects.0 & bc::Effects::WAIT != 0
        }
        _ => false,
    }
}

fn read_id(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn append_id(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

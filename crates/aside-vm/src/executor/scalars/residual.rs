//! Experimental residual Body spans. Only successful ordinary execution teaches
//! a plan; replay retains its original PCs, work grants and clock boundaries.
use super::*;
use crate::class::ClassSlot;
use std::cell::{Cell, RefCell};

mod compile;
mod execute;
mod model;
mod record;
use model::*;

const PLANS: usize = 8;
const STEPS: usize = 32;
const REGISTERS: usize = 64;
const FIELDS: usize = 8;
const MEMOS: usize = 256;
const MEMO_WAYS: usize = 4;

pub(in crate::executor) struct State {
    cache: Option<RefCell<Cache>>,
    spans: Cell<u64>,
    instructions: Cell<u64>,
    pub(in crate::executor) hits: Cell<u64>,
    pub(in crate::executor) guard_fallbacks: Cell<u64>,
    pub(in crate::executor) budget_fallbacks: Cell<u64>,
    pub(in crate::executor) clock_fallbacks: Cell<u64>,
    mode: Cell<Mode>,
    next_span: Cell<usize>,
    pub(in crate::executor) memo_hits: Cell<u64>,
    observation_cell: Cell<Option<Value>>,
}

#[derive(Clone, Copy)]
enum Mode {
    Idle,
    Learning,
    Plan(usize),
}

struct Cache {
    plans: ChargedVec<Plan>,
    recording: Plan,
    facts: ChargedVec<Fact>,
    learning: bool,
    recording_active: bool,
    next: usize,
    memos: ChargedVec<Option<Memo>>,
    next_memo: usize,
    generation: u64,
}

impl State {
    // Fixed-capacity metadata storage is charged at VM construction. Plans hold
    // scalar constants and weak class identities, never owning GC edges.
    pub(in crate::executor) fn new(
        usage: &HeapBudgetUsage,
        enabled: bool,
    ) -> Result<Self, HeapError> {
        let cache = if enabled {
            let mut plans = ChargedVec::new(usage, PLANS)?;
            for _ in 0..PLANS {
                plans.data.push(Plan::new(usage)?);
            }
            let mut facts = ChargedVec::new(usage, REGISTERS)?;
            facts.data.resize(REGISTERS, Fact::Unknown);
            let mut memos = ChargedVec::new(usage, MEMOS)?;
            memos.data.resize(MEMOS, None);
            Some(RefCell::new(Cache {
                plans,
                recording: Plan::new(usage)?,
                facts,
                learning: false,
                recording_active: false,
                next: 0,
                memos,
                next_memo: 0,
                generation: 1,
            }))
        } else {
            None
        };
        Ok(Self {
            cache,
            spans: Cell::new(0),
            instructions: Cell::new(0),
            hits: Cell::new(0),
            guard_fallbacks: Cell::new(0),
            budget_fallbacks: Cell::new(0),
            clock_fallbacks: Cell::new(0),
            mode: Cell::new(Mode::Idle),
            next_span: Cell::new(0),
            memo_hits: Cell::new(0),
            observation_cell: Cell::new(None),
        })
    }

    pub(in crate::executor) fn counts(&self) -> (u64, u64) {
        (self.spans.get(), self.instructions.get())
    }

    fn begin(&self, block: &Block<'_>) {
        let Some(cache) = &self.cache else { return };
        let mut cache = cache.borrow_mut();
        self.mode.set(Mode::Idle);
        self.next_span.set(0);
        self.observation_cell.set(None);
        cache.learning = false;
        cache.recording_active = false;
        let function = block.cursor.function;
        if block.environment.is_none() {
            return;
        }
        #[cfg(test)]
        if block.single || block.general_clears {
            return;
        }
        let f = &block.program.package().functions[function.index()];
        if !block.header.context.derived
            || f.kind != FunctionKind::Plain
            || f.register_count as usize > REGISTERS
            || !f.regions.is_empty()
            || !matches!(f.bindings.first(), Some(binding)
                if !binding.mutable
                    && binding.storage == BindingStorage::Value(Reg(0))
                    && binding.initialization == BindingInitialization::Parameter(0))
        {
            return;
        }
        let Ok(receiver) = block.registers.peek(0) else {
            return;
        };
        if matches!(
            block.registers.peek(f.register_count - 1),
            Err(HeapError::OutOfBounds)
        ) {
            return;
        }
        let slot = memo_slot(function, receiver);
        for offset in 0..MEMO_WAYS {
            let Some(memo) = cache.memos.data[slot + offset] else {
                continue;
            };
            let plan = &cache.plans.data[memo.index];
            if memo.function == function
                && memo.receiver.same_bits(receiver)
                && memo.generation == plan.generation
                && plan.covers(block.cursor.pc)
                && memo.matches(block.registers.heap_read())
            {
                self.mode.set(Mode::Plan(memo.index));
                self.observation_cell.set(Some(memo.observation_cell()));
                self.hits.set(self.hits.get() + 1);
                self.memo_hits.set(self.memo_hits.get() + 1);
                return;
            }
        }
        let candidate = cache
            .plans
            .data
            .iter()
            .any(|plan| plan.function == Some(function) && plan.covers(block.cursor.pc));
        if !candidate && block.cursor.pc != Pc(0) {
            return;
        }
        let Some(resolved) = ResolvedReceiver::read(block.registers.heap_read(), receiver) else {
            if candidate {
                self.guard_fallbacks.set(self.guard_fallbacks.get() + 1);
            }
            return;
        };
        for (index, plan) in cache.plans.data.iter().enumerate() {
            if plan.function == Some(function)
                && plan.covers(block.cursor.pc)
                && let Some(memo) =
                    plan.memo(block.registers.heap_read(), receiver, &resolved, index)
            {
                let target = (0..MEMO_WAYS)
                    .find(|&offset| {
                        cache.memos.data[slot + offset].is_none_or(|old| {
                            old.function == function && old.receiver.same_bits(receiver)
                        })
                    })
                    .unwrap_or(cache.next_memo % MEMO_WAYS);
                cache.next_memo += 1;
                cache.memos.data[slot + target] = Some(memo);
                self.mode.set(Mode::Plan(index));
                self.observation_cell.set(Some(memo.observation_cell()));
                self.hits.set(self.hits.get() + 1);
                return;
            }
        }
        if candidate {
            self.guard_fallbacks.set(self.guard_fallbacks.get() + 1);
        }
        if block.cursor.pc != Pc(0) {
            return;
        }
        cache.recording.reset(function, resolved.proof);
        cache.facts.data.fill(Fact::Unknown);
        cache.facts.data[0] = Fact::Receiver;
        cache.learning = true;
        cache.recording_active = true;
        self.mode.set(Mode::Learning);
    }

    fn finish(&self, successful_prefix: bool) {
        let Some(cache) = &self.cache else { return };
        let mut cache = cache.borrow_mut();
        if cache.recording_active
            && successful_prefix
            && cache.recording.guards.data.len() >= 2
            && cache.recording.steps.data.len() >= 6
        {
            cache.recording.compile();
            cache.recording.generation = cache.generation;
            if let Some(next) = cache.generation.checked_add(1) {
                cache.generation = next;
            } else {
                cache.memos.data.fill(None);
                cache.generation = 1;
            }
            let index = cache.next;
            cache.next = (index + 1) % PLANS;
            let Cache {
                plans, recording, ..
            } = &mut *cache;
            std::mem::swap(&mut plans.data[index], recording);
        }
        self.mode.set(Mode::Idle);
        self.observation_cell.set(None);
        cache.learning = false;
        cache.recording_active = false;
    }
}

fn memo_slot(function: FunctionId, receiver: Value) -> usize {
    let key = receiver.object_parts().map_or(0, |(_, key)| key);
    let hash =
        key.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ u64::from(function.0).wrapping_mul(0x85eb_ca6b);
    ((hash ^ (hash >> 32)) as usize & (MEMOS - 1)) & !(MEMO_WAYS - 1)
}

#[cfg(test)]
mod tests;

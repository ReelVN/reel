//! Bounded scalar instruction fixture. No parser, VM API, scheduling or GC.
mod program;
mod wide;
use crate::*;
pub use program::{animation, story, ui, wide_pressure, Instruction, Op, Program, REGISTERS};
pub use wide::{BoxWide, InlineWide, NoWide, Wide, WideStats};

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct NanSlots(u64);

/// Raw Copy is a borrowed bit copy. Storing another owner requires retain().
pub trait TraceValue: Copy {
    type Store: Wide;
    const NAME: &'static str;
    fn encode(input: Input, store: &mut Self::Store) -> Self;
    fn kind(self) -> Kind;
    fn integer(self, store: &Self::Store) -> Result<i64, Error>;
    fn float(self) -> f64;
    fn boolean(self) -> bool;
    fn object(self) -> Handle;
    fn wide_handle(self) -> Option<Handle>;
    fn retain(self, store: &mut Self::Store) {
        if let Some(handle) = self.wide_handle() {
            assert!(store.retain(handle));
        }
    }
    fn release(self, store: &mut Self::Store) {
        if let Some(handle) = self.wide_handle() {
            assert!(store.release(handle));
        }
    }
    fn decode(self, store: &Self::Store) -> Result<Input, Error> {
        Ok(match self.kind() {
            Kind::Null => Input::Null,
            Kind::Bool => Input::Bool(self.boolean()),
            Kind::Int => Input::Int(self.integer(store)?),
            Kind::Float => Input::Float(self.float()),
            Kind::Object => Input::Object(self.object().bits()),
        })
    }
}

impl TraceValue for Explicit {
    type Store = NoWide;
    const NAME: &'static str = "explicit16";
    fn encode(input: Input, _: &mut NoWide) -> Self {
        let (tag, payload) = match input {
            Input::Null => (Tag::Null, 0),
            Input::Bool(v) => (Tag::Bool, v as u64),
            Input::Int(v) => (Tag::Int, v as u64),
            Input::Float(v) => (Tag::Float, v.to_bits()),
            Input::Object(bits) => (Tag::Object, bits),
        };
        Self { tag, payload }
    }
    fn kind(self) -> Kind {
        Representation::kind(self)
    }
    fn integer(self, _: &NoWide) -> Result<i64, Error> {
        Ok(self.payload as i64)
    }
    fn float(self) -> f64 {
        f64::from_bits(self.payload)
    }
    fn boolean(self) -> bool {
        self.payload != 0
    }
    fn object(self) -> Handle {
        Handle(self.payload)
    }
    fn wide_handle(self) -> Option<Handle> {
        None
    }
}

fn encode_nan<W: Wide>(input: Input, store: &mut W) -> u64 {
    match input {
        Input::Null => NULL,
        Input::Bool(v) => BOOL | v as u64,
        Input::Int(v) if (SMALL_MIN..=SMALL_MAX).contains(&v) => INT | (v as u64 & PAYLOAD_MASK),
        Input::Int(v) => WIDE | store.allocate(v).bits(),
        Input::Float(v) => normalized_float_bits(v),
        Input::Object(bits) => {
            assert!(bits <= PAYLOAD_MASK);
            OBJECT | bits
        }
    }
}
fn nan_integer<W: Wide>(bits: u64, store: &W) -> Result<i64, Error> {
    if bits & !PAYLOAD_MASK == WIDE {
        store
            .get(Handle(bits & PAYLOAD_MASK))
            .ok_or(Error::InvalidHandle)
    } else {
        Ok(((bits << 16) as i64) >> 16)
    }
}
fn nan_wide(bits: u64) -> Option<Handle> {
    (bits & !PAYLOAD_MASK == WIDE).then_some(Handle(bits & PAYLOAD_MASK))
}

macro_rules! nan_trace {
    ($value:ty, $store:ty, $name:literal) => {
        impl TraceValue for $value {
            type Store = $store;
            const NAME: &'static str = $name;
            fn encode(input: Input, store: &mut Self::Store) -> Self {
                Self(encode_nan(input, store))
            }
            fn kind(self) -> Kind {
                Representation::kind(NanBox(self.0))
            }
            fn integer(self, store: &Self::Store) -> Result<i64, Error> {
                nan_integer(self.0, store)
            }
            fn float(self) -> f64 {
                f64::from_bits(self.0)
            }
            fn boolean(self) -> bool {
                self.0 & PAYLOAD_MASK != 0
            }
            fn object(self) -> Handle {
                Handle(self.0 & PAYLOAD_MASK)
            }
            fn wide_handle(self) -> Option<Handle> {
                nan_wide(self.0)
            }
        }
    };
}
nan_trace!(NanBox, BoxWide, "nanbox8_box_rc");
nan_trace!(NanSlots, InlineWide, "nanbox8_slots_rc");
const _: () = assert!(size_of::<NanSlots>() == 8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NumericType,
    BooleanType,
    ObjectType,
    MixedComparison,
    Arithmetic,
    InvalidHandle,
    Budget,
}

struct HostObject {
    fields: [Input; 4],
}
struct HostHeap {
    objects: Arena<HostObject>,
    root: Handle,
    calls: u64,
    digest: u64,
}
impl HostHeap {
    fn new(fields: [Input; 4]) -> Self {
        let mut objects = Arena::default();
        let root = objects.insert(HostObject { fields });
        Self {
            objects,
            root,
            calls: 0,
            digest: 0,
        }
    }
    // These exact native paths are shared by every Value representation.
    #[inline(never)]
    fn get(&self, object: Handle, field: usize) -> Result<Input, Error> {
        let object = self.objects.get(object).ok_or(Error::InvalidHandle)?;
        Ok(object.fields[field])
    }
    #[inline(never)]
    fn set(&mut self, object: Handle, field: usize, value: Input) -> Result<(), Error> {
        let object = self.objects.get_mut(object).ok_or(Error::InvalidHandle)?;
        object.fields[field] = value;
        Ok(())
    }
    #[inline(never)]
    fn call(&mut self, operation: u32, argument: Input) -> Result<Input, Error> {
        let result = match (operation, argument) {
            (0, value) => value,
            (1, Input::Int(v)) => Input::Bool(v > 0),
            (2, Input::Float(v)) => Input::Float(v.clamp(0.0, 1.0)),
            _ => return Err(Error::NumericType),
        };
        self.calls += 1;
        self.digest = program::host_digest_step(self.digest, argument, operation);
        Ok(result)
    }
}

pub struct State<V: TraceValue> {
    registers: Vec<V>,
    constants: Vec<V>,
    wide: V::Store,
    host: HostHeap,
}

#[derive(Clone, Copy, Debug)]
pub struct Storage {
    pub value_bytes: usize,
    pub host_table_bytes: usize,
    pub host_box_bytes: usize,
    pub wide: WideStats,
}

fn logical(input: Input) -> (Kind, u64) {
    match input {
        Input::Null => (Kind::Null, 0),
        Input::Bool(v) => (Kind::Bool, v as u64),
        Input::Int(v) => (Kind::Int, v as u64),
        Input::Float(v) => (Kind::Float, normalized_float_bits(v)),
        Input::Object(v) => (Kind::Object, v),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub registers: Vec<(Kind, u64)>,
    pub fields: [(Kind, u64); 4],
    pub host_calls: u64,
    pub host_digest: u64,
    pub execution: Execution,
}

impl<V: TraceValue> State<V> {
    pub fn new(program: &Program) -> Self {
        let host = HostHeap::new(program.initial_fields);
        let mut wide = V::Store::default();
        let mut constants = Vec::with_capacity(program.literals.len());
        for &input in &program.literals {
            let input = match input {
                Input::Object(index) => {
                    assert_eq!(index, 0);
                    Input::Object(host.root.bits())
                }
                other => other,
            };
            constants.push(V::encode(input, &mut wide));
        }
        let registers = vec![V::encode(Input::Null, &mut wide); REGISTERS];
        Self {
            registers,
            constants,
            wide,
            host,
        }
    }
    fn assign_owned(&mut self, destination: usize, value: V) {
        self.registers[destination].release(&mut self.wide);
        self.registers[destination] = value;
    }
    fn assign_scalar(&mut self, destination: usize, value: Input) {
        let owned = V::encode(value, &mut self.wide);
        self.assign_owned(destination, owned);
    }
    fn copy_value(&mut self, destination: usize, value: V) {
        // Retain first: copying a register to itself preserves its sole owner.
        value.retain(&mut self.wide);
        self.assign_owned(destination, value);
    }
    fn number(&self, register: usize) -> Result<Number, Error> {
        let value = self.registers[register];
        match value.kind() {
            Kind::Int => Ok(Number::Int(value.integer(&self.wide)?)),
            Kind::Float => Ok(Number::Float(value.float())),
            _ => Err(Error::NumericType),
        }
    }
    fn boolean(&self, register: usize) -> Result<bool, Error> {
        let value = self.registers[register];
        if value.kind() != Kind::Bool {
            return Err(Error::BooleanType);
        }
        Ok(value.boolean())
    }
    fn object(&self, register: usize) -> Result<Handle, Error> {
        let value = self.registers[register];
        if value.kind() != Kind::Object {
            return Err(Error::ObjectType);
        }
        Ok(value.object())
    }
    pub fn storage(&self) -> Storage {
        let host = self.host.objects.storage();
        Storage {
            value_bytes: (self.registers.capacity() + self.constants.capacity()) * size_of::<V>(),
            host_table_bytes: host.table_bytes,
            host_box_bytes: host.box_bytes,
            wide: self.wide.stats(),
        }
    }
    pub fn snapshot(&self, execution: Execution) -> Snapshot {
        Snapshot {
            registers: self
                .registers
                .iter()
                .map(|&v| logical(v.decode(&self.wide).unwrap()))
                .collect(),
            fields: self
                .host
                .objects
                .get(self.host.root)
                .unwrap()
                .fields
                .map(logical),
            host_calls: self.host.calls,
            host_digest: self.host.digest,
            execution,
        }
    }
    pub fn close(mut self) -> WideStats {
        for &value in &self.registers {
            value.release(&mut self.wide);
        }
        for &value in &self.constants {
            value.release(&mut self.wide);
        }
        let stats = self.wide.stats();
        assert_eq!(stats.live, 0);
        assert_eq!(stats.box_allocations, stats.box_deallocations);
        drop(self); // includes Value arrays, native host object and slot tables
        stats
    }
}

#[derive(Clone, Copy)]
enum Number {
    Int(i64),
    Float(f64),
}
impl Number {
    fn float(self) -> f64 {
        match self {
            Self::Int(v) => v as f64,
            Self::Float(v) => v,
        }
    }
    fn binary(self, rhs: Self, operation: Op) -> Result<Input, Error> {
        if let (Self::Int(a), Self::Int(b)) = (self, rhs) {
            let result = match operation {
                Op::Add => a.checked_add(b),
                Op::Sub => a.checked_sub(b),
                Op::Mul => a.checked_mul(b),
                Op::Rem => a.checked_rem(b),
                _ => unreachable!(),
            };
            return result.map(Input::Int).ok_or(Error::Arithmetic);
        }
        let (a, b) = (self.float(), rhs.float());
        Ok(Input::Float(match operation {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Rem => return Err(Error::NumericType),
            _ => unreachable!(),
        }))
    }
    fn less(self, rhs: Self) -> Result<bool, Error> {
        match (self, rhs) {
            (Self::Int(a), Self::Int(b)) => Ok(a < b),
            (Self::Float(a), Self::Float(b)) => Ok(a < b),
            _ => Err(Error::MixedComparison), // trace subset; no approximate int/float comparison
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Execution {
    pub instructions: u64,
    pub type_checks: u64,
    pub copies: u64,
    pub branches_taken: u64,
}

/// Same generic scalar fetch/dispatch loop and dynamic type checks for E/N.
/// No vectorization flags or per-op black_box to bias one representation.
#[inline(never)]
pub fn execute<V: TraceValue>(program: &Program, state: &mut State<V>) -> Result<Execution, Error> {
    let code = std::hint::black_box(program.code.as_slice());
    let mut pc = 0usize;
    let mut executed = Execution::default();
    loop {
        if executed.instructions >= program.budget {
            return Err(Error::Budget);
        }
        let instruction = code[pc];
        pc += 1;
        executed.instructions += 1;
        let (a, b, c, immediate) = (
            instruction.a as usize,
            instruction.b as usize,
            instruction.c as usize,
            instruction.immediate as usize,
        );
        match instruction.op {
            Op::Load => {
                state.copy_value(a, state.constants[immediate]);
                executed.copies += 1;
            }
            Op::Copy => {
                state.copy_value(a, state.registers[b]);
                executed.copies += 1;
            }
            Op::Add | Op::Sub | Op::Mul | Op::Rem => {
                executed.type_checks += 2;
                let result = state.number(b)?.binary(state.number(c)?, instruction.op)?;
                state.assign_scalar(a, result);
            }
            Op::Less => {
                executed.type_checks += 2;
                let result = state.number(b)?.less(state.number(c)?)?;
                state.assign_scalar(a, Input::Bool(result));
            }
            Op::JumpIf => {
                executed.type_checks += 1;
                if state.boolean(a)? {
                    pc = immediate;
                    executed.branches_taken += 1;
                }
            }
            Op::Jump => {
                pc = immediate;
            }
            Op::GetField => {
                executed.type_checks += 1;
                let result = state.host.get(state.object(b)?, immediate)?;
                state.assign_scalar(a, result);
            }
            Op::SetField => {
                executed.type_checks += 2;
                state.host.set(
                    state.object(a)?,
                    immediate,
                    state.registers[b].decode(&state.wide)?,
                )?;
            }
            Op::Host => {
                executed.type_checks += 1;
                let argument = state.registers[b].decode(&state.wide)?;
                let result = state.host.call(instruction.immediate, argument)?;
                state.assign_scalar(a, result);
            }
            Op::LoadIndexed => {
                executed.type_checks += 1;
                let Number::Int(index) = state.number(b)? else {
                    return Err(Error::NumericType);
                };
                let index = usize::try_from(index).map_err(|_| Error::Arithmetic)?;
                state.copy_value(a, state.constants[immediate + index]);
                executed.copies += 1;
            }
            Op::Halt => return Ok(executed),
        }
    }
}

pub fn validate_snapshot(program: &Program, snapshot: &Snapshot) {
    assert_eq!(snapshot.fields, program.expected_fields.map(logical));
    assert_eq!(snapshot.host_calls, program.expected_calls);
    assert_eq!(snapshot.host_digest, program.expected_host_digest);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run<V: TraceValue>(program: &Program) -> Snapshot {
        let mut state = State::<V>::new(program);
        let execution = execute(program, &mut state).unwrap();
        let snapshot = state.snapshot(execution);
        validate_snapshot(program, &snapshot);
        state.close();
        snapshot
    }
    #[test]
    fn all_traces_match_native_oracles_and_full_register_snapshots() {
        for seed in [1, 42, 0x9e37_79b9_7f4a_7c15] {
            for cycles in [1, 65, 4096] {
                let mut programs = vec![
                    story(cycles, seed),
                    ui(cycles, seed),
                    animation(cycles, seed),
                ];
                for percent in [0, 1, 10, 50, 100] {
                    programs.push(wide_pressure(cycles, seed, percent));
                }
                for program in programs {
                    let baseline = run::<Explicit>(&program);
                    assert_eq!(run::<NanBox>(&program), baseline);
                    assert_eq!(run::<NanSlots>(&program), baseline);
                }
            }
        }
    }
    fn copy_boundary<V: TraceValue>() {
        let program = story(1, 1);
        let mut state = State::<V>::new(&program);
        for value in [i64::MIN, i64::MAX, SMALL_MIN - 1, SMALL_MAX + 1] {
            state.assign_scalar(0, Input::Int(value));
            state.copy_value(1, state.registers[0]);
            state.copy_value(0, state.registers[0]); // self-copy
            state.assign_scalar(0, Input::Null);
            assert_eq!(state.registers[1].integer(&state.wide), Ok(value));
            state.assign_scalar(1, Input::Null);
            assert_eq!(state.wide.stats().live, 0);
        }
        state.close();
    }
    #[test]
    fn wide_copy_lifetime_and_int64_endpoints() {
        copy_boundary::<Explicit>();
        copy_boundary::<NanBox>();
        copy_boundary::<NanSlots>();
    }
    #[test]
    fn checked_types_overflow_and_budget_remain_observable() {
        fn check<V: TraceValue>(program: &Program, expected: Error) {
            let mut state = State::<V>::new(program);
            assert_eq!(execute(program, &mut state), Err(expected));
            state.close();
        }
        fn check_all(program: &Program, expected: Error) {
            check::<Explicit>(program, expected);
            check::<NanBox>(program, expected);
            check::<NanSlots>(program, expected);
        }
        for value in [Input::Bool(true), Input::Null, Input::Object(0)] {
            let mut p = story(1, 42);
            p.literals[0] = value;
            check_all(&p, Error::NumericType);
        }
        let mut p = story(1, 42);
        p.literals[3] = Input::Bool(true);
        check_all(&p, Error::ObjectType);
        let mut p = story(1, 42);
        p.code[0].op = Op::JumpIf;
        check_all(&p, Error::BooleanType);
        let mut p = story(1, 42);
        p.initial_fields[0] = Input::Int(1);
        p.literals[4] = Input::Int(i64::MAX);
        check_all(&p, Error::Arithmetic);
        assert_eq!(
            Number::Int(i64::MAX)
                .binary(Number::Int(1), Op::Add)
                .unwrap_err(),
            Error::Arithmetic
        );
        assert_eq!(
            Number::Int(1).binary(Number::Int(0), Op::Rem).unwrap_err(),
            Error::Arithmetic
        );
        assert_eq!(
            Number::Int(1).less(Number::Float(1.0)),
            Err(Error::MixedComparison)
        );
        let mut p = animation(65, 42);
        p.budget = 1;
        check_all(&p, Error::Budget);
    }
    #[test]
    fn trace_encodings_preserve_boundaries_and_normalize_candidate_nans() {
        fn check<V: TraceValue>() {
            let mut wide = V::Store::default();
            let mut inputs = vec![
                Input::Null,
                Input::Bool(false),
                Input::Bool(true),
                Input::Object(Handle::new(FIELD_MAX, FIELD_MAX).unwrap().bits()),
            ];
            for value in [
                i64::MIN,
                i64::MAX,
                SMALL_MIN - 1,
                SMALL_MIN,
                SMALL_MAX,
                SMALL_MAX + 1,
            ] {
                inputs.push(Input::Int(value));
            }
            for bits in [
                0,
                1 << 63,
                1,
                0x7ff0_0000_0000_0001,
                0xffff_ffff_ffff_ffff,
                WIDE | PAYLOAD_MASK,
                OBJECT | PAYLOAD_MASK,
                CANONICAL_NAN,
            ] {
                inputs.push(Input::Float(f64::from_bits(bits)));
            }
            let mut random = 42;
            for _ in 0..10_000 {
                let bits = random_word(&mut random);
                inputs.push(Input::Int(bits as i64));
                inputs.push(Input::Float(f64::from_bits(bits)));
            }
            for input in inputs {
                let value = V::encode(input, &mut wide);
                assert_eq!(logical(value.decode(&wide).unwrap()), logical(input));
                if let Input::Float(float) = input {
                    assert_eq!(
                        value.float().to_bits(),
                        if V::NAME == "explicit16" {
                            float.to_bits()
                        } else {
                            normalized_float_bits(float)
                        }
                    );
                }
                value.release(&mut wide);
                assert_eq!(wide.stats().live, 0);
            }
            assert_eq!(wide.stats().box_allocations, wide.stats().box_deallocations);
        }
        check::<Explicit>();
        check::<NanBox>();
        check::<NanSlots>();
    }
}

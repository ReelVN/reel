use crate::{input_digest, random_word, Input};

pub const REGISTERS: usize = 32;

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum Op {
    Load,
    Copy,
    Add,
    Sub,
    Mul,
    Rem,
    Less,
    JumpIf,
    Jump,
    GetField,
    SetField,
    Host,
    LoadIndexed,
    Halt,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Instruction {
    pub op: Op,
    pub a: u16,
    pub b: u16,
    pub c: u16,
    pub immediate: u32,
}

pub struct Program {
    pub name: String,
    pub code: Vec<Instruction>,
    pub literals: Vec<Input>,
    pub initial_fields: [Input; 4],
    pub expected_fields: [Input; 4],
    pub expected_calls: u64,
    pub expected_host_digest: u64,
    pub cycles: usize,
    pub budget: u64,
    pub wide_pool_count: usize,
}

pub fn host_digest_step(previous: u64, input: Input, operation: u32) -> u64 {
    previous
        .rotate_left(9)
        .wrapping_add(input_digest(input) ^ operation as u64)
}

#[derive(Default)]
struct Builder {
    code: Vec<Instruction>,
    literals: Vec<Input>,
}
struct Expected {
    fields: [Input; 4],
    calls: u64,
    digest: u64,
}
impl Builder {
    fn emit(&mut self, op: Op, a: u16, b: u16, c: u16, immediate: u32) -> usize {
        let position = self.code.len();
        self.code.push(Instruction {
            op,
            a,
            b,
            c,
            immediate,
        });
        position
    }
    fn load(&mut self, register: u16, input: Input) {
        let constant = self.literals.len() as u32;
        self.literals.push(input);
        self.emit(Op::Load, register, 0, 0, constant);
    }
    fn jump(&mut self, op: Op, condition: u16) -> usize {
        self.emit(op, condition, 0, 0, 0)
    }
    fn patch(&mut self, position: usize) {
        self.code[position].immediate = self.code.len() as u32;
    }
    fn setup(&mut self, cycles: usize) {
        assert!((1..=1_048_576).contains(&cycles));
        self.load(0, Input::Int(cycles as i64));
        self.load(1, Input::Int(1));
        self.load(2, Input::Int(0));
        self.load(3, Input::Object(0)); // externally rooted host object index
    }
    fn gated_host(&mut self, argument: u16, result: u16, operation: u32, field: u32) {
        self.emit(Op::Rem, 15, 0, 14, 0);
        self.emit(Op::Less, 16, 15, 1, 0);
        let call = self.jump(Op::JumpIf, 16);
        let skip = self.jump(Op::Jump, 0);
        self.patch(call);
        self.emit(Op::Host, result, argument, 0, operation);
        self.emit(Op::SetField, 3, result, 0, field);
        self.patch(skip);
    }
    fn finish_loop(&mut self, start: usize) {
        self.emit(Op::Sub, 0, 0, 1, 0);
        self.emit(Op::Less, 31, 2, 0, 0);
        self.emit(Op::JumpIf, 31, 0, 0, start as u32);
        self.emit(Op::Halt, 0, 0, 0, 0);
    }
    fn finish(
        mut self,
        name: String,
        cycles: usize,
        initial_fields: [Input; 4],
        expected: Expected,
        wide_pool_count: usize,
        seed: u64,
    ) -> Program {
        // Runtime register numbering, shared by every representation.
        let mut map: Vec<u16> = (0..REGISTERS as u16).collect();
        let mut state = seed;
        for i in (1..map.len()).rev() {
            let j = random_word(&mut state) as usize % (i + 1);
            map.swap(i, j);
        }
        for instruction in &mut self.code {
            instruction.a = map[instruction.a as usize];
            instruction.b = map[instruction.b as usize];
            instruction.c = map[instruction.c as usize];
        }
        Program {
            name,
            code: self.code,
            literals: self.literals,
            initial_fields,
            expected_fields: expected.fields,
            expected_calls: expected.calls,
            expected_host_digest: expected.digest,
            cycles,
            budget: cycles as u64 * 64 + 128,
            wide_pool_count,
        }
    }
}

pub fn story(cycles: usize, seed: u64) -> Program {
    let mut rng = seed;
    let start = (random_word(&mut rng) % 99) as i64;
    let increment = (random_word(&mut rng) % 7 + 1) as i64;
    let threshold = (random_word(&mut rng) % 1000 + 1000) as i64;
    let initial = [
        Input::Int(start),
        Input::Bool(false),
        Input::Null,
        Input::Null,
    ];
    let mut b = Builder::default();
    b.setup(cycles);
    b.load(4, Input::Int(increment));
    b.load(5, Input::Int(threshold));
    b.load(11, Input::Bool(false));
    let false_constant = b.literals.len() - 1;
    let loop_start = b.code.len();
    b.emit(Op::GetField, 6, 3, 0, 0);
    b.emit(Op::Copy, 7, 6, 0, 0);
    b.emit(Op::Add, 6, 7, 4, 0);
    b.emit(Op::Less, 8, 5, 6, 0);
    let reset = b.jump(Op::JumpIf, 8);
    b.emit(Op::Load, 11, 0, 0, false_constant as u32);
    b.emit(Op::SetField, 3, 6, 0, 0);
    let common = b.jump(Op::Jump, 0);
    b.patch(reset);
    b.emit(Op::Sub, 6, 6, 5, 0);
    b.emit(Op::SetField, 3, 6, 0, 0);
    b.emit(Op::Host, 11, 6, 0, 1);
    b.patch(common);
    b.emit(Op::SetField, 3, 11, 0, 1);
    b.emit(Op::Copy, 13, 11, 0, 0);
    b.finish_loop(loop_start);
    let mut score = start;
    let mut flag = false;
    let mut calls = 0;
    let mut digest = 0;
    // Independent native state transition oracle, outside the timed interpreter.
    for _ in 0..cycles {
        score += increment;
        flag = score > threshold;
        if flag {
            score -= threshold;
            calls += 1;
            digest = host_digest_step(digest, Input::Int(score), 1);
        }
    }
    b.finish(
        "story_branch".into(),
        cycles,
        initial,
        Expected {
            fields: [
                Input::Int(score),
                Input::Bool(flag && score > 0),
                Input::Null,
                Input::Null,
            ],
            calls,
            digest,
        },
        0,
        seed,
    )
}

pub fn ui(cycles: usize, seed: u64) -> Program {
    let mut rng = seed;
    let start = (random_word(&mut rng) % 1000) as f64 / 2500.0;
    let scale = 0.97;
    let delta = 0.013;
    let threshold = 0.4;
    let width = (random_word(&mut rng) % 500 + 300) as f64;
    let offset = (random_word(&mut rng) % 20) as f64;
    let initial = [
        Input::Float(start),
        Input::Float(0.0),
        Input::Float(0.0),
        Input::Null,
    ];
    let mut b = Builder::default();
    b.setup(cycles);
    for (reg, value) in [
        (4, scale),
        (5, delta),
        (10, threshold),
        (18, width),
        (19, offset),
    ] {
        b.load(reg, Input::Float(value));
    }
    b.load(14, Input::Int(64));
    let loop_start = b.code.len();
    b.emit(Op::GetField, 6, 3, 0, 0);
    b.emit(Op::Mul, 7, 6, 4, 0);
    b.emit(Op::Add, 8, 7, 5, 0);
    b.emit(Op::Less, 9, 8, 10, 0);
    let no_wrap = b.jump(Op::JumpIf, 9);
    b.emit(Op::Sub, 8, 8, 10, 0);
    b.patch(no_wrap);
    b.emit(Op::Copy, 11, 8, 0, 0);
    b.emit(Op::SetField, 3, 11, 0, 0);
    b.emit(Op::Mul, 12, 11, 18, 0);
    b.emit(Op::Add, 13, 12, 19, 0);
    b.emit(Op::SetField, 3, 13, 0, 1);
    b.gated_host(11, 20, 2, 2);
    b.finish_loop(loop_start);
    let mut progress = start;
    let mut display = 0.0;
    let mut host_result = 0.0;
    let mut digest = 0;
    for remaining in (1..=cycles).rev() {
        progress = progress * scale + delta;
        if progress >= threshold {
            progress -= threshold;
        }
        display = progress * width + offset;
        if remaining % 64 == 0 {
            host_result = progress.clamp(0.0, 1.0);
            digest = host_digest_step(digest, Input::Float(progress), 2);
        }
    }
    b.finish(
        "ui_properties".into(),
        cycles,
        initial,
        Expected {
            fields: [
                Input::Float(progress),
                Input::Float(display),
                Input::Float(host_result),
                Input::Null,
            ],
            calls: (cycles / 64) as u64,
            digest,
        },
        0,
        seed,
    )
}

pub fn animation(cycles: usize, seed: u64) -> Program {
    let mut rng = seed;
    let start = (random_word(&mut rng) % 1000) as f64 / 1000.0;
    let velocity = (random_word(&mut rng) % 201) as f64 / 100.0 - 1.0;
    let dt = 1.0 / 60.0;
    let initial = [
        Input::Float(start),
        Input::Float(velocity),
        Input::Float(0.0),
        Input::Null,
    ];
    let mut b = Builder::default();
    b.setup(cycles);
    b.load(4, Input::Float(dt));
    b.load(5, Input::Float(1.0));
    b.load(10, Input::Float(0.0));
    b.load(14, Input::Int(64));
    let loop_start = b.code.len();
    b.emit(Op::GetField, 6, 3, 0, 0);
    b.emit(Op::Copy, 11, 6, 0, 0);
    b.emit(Op::GetField, 7, 3, 0, 1);
    b.emit(Op::Mul, 8, 7, 4, 0);
    b.emit(Op::Add, 6, 11, 8, 0);
    b.emit(Op::Less, 9, 5, 6, 0);
    let wrap_high = b.jump(Op::JumpIf, 9);
    let check_low = b.jump(Op::Jump, 0);
    b.patch(wrap_high);
    b.emit(Op::Sub, 6, 6, 5, 0);
    b.patch(check_low);
    b.emit(Op::Less, 9, 6, 10, 0);
    let wrap_low = b.jump(Op::JumpIf, 9);
    let common = b.jump(Op::Jump, 0);
    b.patch(wrap_low);
    b.emit(Op::Add, 6, 6, 5, 0);
    b.patch(common);
    b.emit(Op::SetField, 3, 6, 0, 0);
    b.gated_host(6, 20, 2, 2);
    b.finish_loop(loop_start);
    let mut position = start;
    let mut host_result = 0.0;
    let mut digest = 0;
    for remaining in (1..=cycles).rev() {
        position += velocity * dt;
        if position > 1.0 {
            position -= 1.0;
        }
        if position < 0.0 {
            position += 1.0;
        }
        if remaining % 64 == 0 {
            host_result = position.clamp(0.0, 1.0);
            digest = host_digest_step(digest, Input::Float(position), 2);
        }
    }
    b.finish(
        "float_animation".into(),
        cycles,
        initial,
        Expected {
            fields: [
                Input::Float(position),
                Input::Float(velocity),
                Input::Float(host_result),
                Input::Null,
            ],
            calls: (cycles / 64) as u64,
            digest,
        },
        0,
        seed,
    )
}

pub fn wide_pressure(cycles: usize, seed: u64, percent: usize) -> Program {
    assert!(percent <= 100);
    const POOL: usize = 1024;
    let wide_count = POOL * percent / 100;
    let mut rng = seed;
    let mut pool = Vec::with_capacity(POOL);
    for i in 0..POOL {
        let word = random_word(&mut rng);
        let value = if i < wide_count {
            (1i64 << 50) + (word % 1_000_000) as i64
        } else {
            (word & 0xffff) as i64
        };
        pool.push(if word & (1 << 63) == 0 { value } else { -value });
    }
    for i in (1..POOL).rev() {
        let j = random_word(&mut rng) as usize % (i + 1);
        pool.swap(i, j);
    }
    let initial = [Input::Int(0), Input::Null, Input::Null, Input::Null];
    let mut b = Builder::default();
    b.setup(cycles);
    b.load(4, Input::Int(0));
    b.load(5, Input::Int(POOL as i64));
    b.load(14, Input::Int(64));
    let pool_start = b.literals.len() as u32;
    b.literals.extend(pool.iter().copied().map(Input::Int));
    let loop_start = b.code.len();
    b.emit(Op::LoadIndexed, 6, 4, 0, pool_start);
    b.emit(Op::Copy, 7, 6, 0, 0);
    b.emit(Op::Add, 8, 7, 1, 0);
    b.emit(Op::SetField, 3, 8, 0, 0);
    b.emit(Op::GetField, 9, 3, 0, 0);
    b.emit(Op::Copy, 10, 9, 0, 0);
    b.emit(Op::Less, 11, 2, 10, 0);
    let positive = b.jump(Op::JumpIf, 11);
    b.emit(Op::Copy, 12, 8, 0, 0);
    let common = b.jump(Op::Jump, 0);
    b.patch(positive);
    b.emit(Op::Copy, 12, 10, 0, 0);
    b.patch(common);
    b.gated_host(12, 17, 0, 1);
    b.emit(Op::Add, 4, 4, 1, 0);
    b.emit(Op::Rem, 4, 4, 5, 0);
    b.finish_loop(loop_start);
    let mut output = 0;
    let mut host_result = Input::Null;
    let mut digest = 0;
    for i in 0..cycles {
        output = pool[i % POOL] + 1;
        if (cycles - i).is_multiple_of(64) {
            host_result = Input::Int(output);
            digest = host_digest_step(digest, host_result, 0);
        }
    }
    b.finish(
        format!("wide_trace_{percent:03}pct"),
        cycles,
        initial,
        Expected {
            fields: [Input::Int(output), host_result, Input::Null, Input::Null],
            calls: (cycles / 64) as u64,
            digest,
        },
        wide_count,
        seed,
    )
}

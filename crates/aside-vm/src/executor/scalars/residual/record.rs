use super::*;

impl State {
    pub(super) fn learn(&self, block: &Block<'_>, decoded: BodyInstruction) {
        let Some(cache) = &self.cache else { return };
        let mut cache = cache.borrow_mut();
        if !cache.learning {
            return;
        }
        if cache.recording.steps.data.len() == STEPS {
            return;
        }
        let pc = block.cursor.pc;
        let [a, b, c] = decoded.instruction.operands;
        let op = decoded.opcode;
        let fact = |register: u32| {
            cache
                .facts
                .data
                .get(register as usize)
                .copied()
                .unwrap_or(Fact::Unknown)
        };
        let input = |register: u32, expected| Some(Input { register, expected });
        let mut inputs = [None; 2];
        let mut target = decoded.next;
        let mut work = 1;
        let effect = match op {
            Opcode::Clear | Opcode::Clear2 | Opcode::Clear3 => {
                let count = match op {
                    Opcode::Clear => 1,
                    Opcode::Clear2 => 2,
                    _ => 3,
                };
                if [a, b, c][..count].contains(&0) {
                    cache.learning = false;
                    return;
                }
                work = count as u64;
                Effect::Clear([a, b, c], count as u8)
            }
            Opcode::Jump | Opcode::BranchBool | Opcode::BranchNull => {
                if op != Opcode::Jump {
                    let Fact::Scalar(value) = fact(a) else {
                        cache.learning = false;
                        return;
                    };
                    inputs[0] = input(a, Fact::Scalar(value));
                }
                let Ok(next) = branch_target(op, [a, b, c], |register| {
                    let Fact::Scalar(value) = fact(register) else {
                        return Err(FaultKind::InvalidState);
                    };
                    Ok(value)
                }) else {
                    cache.learning = false;
                    return;
                };
                if next <= pc {
                    cache.learning = false;
                    return;
                }
                target = next;
                Effect::Branch
            }
            Opcode::MemberGet if matches!(fact(b), Fact::Receiver) => {
                let Some((guard, value, field_work)) = readonly_field(block, b, SymbolId(c)) else {
                    cache.learning = false;
                    return;
                };
                if cache.recording.guards.data.len() == FIELDS {
                    cache.learning = false;
                    return;
                }
                cache.recording.guards.data.push(guard);
                inputs[0] = input(b, Fact::Receiver);
                work = field_work;
                Effect::Write(a, value)
            }
            Opcode::LoadConst => {
                let value = match block.program.package().constants[b as usize] {
                    Const::Null => Value::NULL,
                    Const::Bool(value) => Value::bool(value),
                    Const::Int(value) => Value::int(value),
                    Const::FloatBits(bits) => Value::float(f64::from_bits(bits)),
                    _ => {
                        cache.learning = false;
                        return;
                    }
                };
                Effect::Write(a, value)
            }
            Opcode::Move | Opcode::BindRead => {
                let source = if op == Opcode::Move {
                    b
                } else {
                    match block.program.package().functions[block.cursor.function.index()].bindings
                        [b as usize]
                        .storage
                    {
                        BindingStorage::Value(register) => register.0,
                        _ => {
                            cache.learning = false;
                            return;
                        }
                    }
                };
                match fact(source) {
                    Fact::Scalar(value) => {
                        inputs[0] = input(source, Fact::Scalar(value));
                        Effect::Write(a, value)
                    }
                    other => {
                        if a == 0 {
                            cache.learning = false;
                            return;
                        }
                        cache.facts.data[a as usize] = other;
                        return;
                    }
                }
            }
            Opcode::BindInit | Opcode::BindWrite => {
                let storage = block.program.package().functions[block.cursor.function.index()]
                    .bindings[a as usize]
                    .storage;
                let BindingStorage::Value(register) = storage else {
                    cache.learning = false;
                    return;
                };
                if register.0 == 0 {
                    cache.learning = false;
                    return;
                }
                cache.facts.data[register.index()] = Fact::Unknown;
                return;
            }
            _ if supported(op) || matches!(op, Opcode::Eq | Opcode::Ne) => {
                let Fact::Scalar(left) = fact(b) else {
                    if a == 0 {
                        cache.learning = false;
                        return;
                    }
                    cache.facts.data[a as usize] = Fact::Unknown;
                    return;
                };
                inputs[0] = input(b, Fact::Scalar(left));
                let result = match op {
                    Opcode::EqNumber => Ok(Value::bool(number_matches(
                        left,
                        &block.program.package().constants[c as usize],
                    ))),
                    Opcode::Neg | Opcode::Pos | Opcode::Not | Opcode::BitNot => unary(op, left),
                    _ => {
                        let Fact::Scalar(right) = fact(c) else {
                            if a == 0 {
                                cache.learning = false;
                                return;
                            }
                            cache.facts.data[a as usize] = Fact::Unknown;
                            return;
                        };
                        inputs[1] = input(c, Fact::Scalar(right));
                        binary(op, left, right)
                    }
                };
                let Ok(value) = result else {
                    cache.learning = false;
                    return;
                };
                Effect::Write(a, value)
            }
            _ => {
                cache.learning = false;
                return;
            }
        };
        let candidate = Step {
            pc,
            end: decoded.next,
            target,
            opcode: op,
            work,
            inputs,
            effect,
        };
        for (register, value) in candidate.destinations() {
            if register == 0 {
                cache.learning = false;
                return;
            }
            cache.facts.data[register as usize] = value.map_or(Fact::Unknown, Fact::Scalar);
        }
        cache.recording.steps.data.push(candidate);
    }
}

fn readonly_field(block: &Block<'_>, source: u32, name: SymbolId) -> Option<(Guard, Value, u64)> {
    let receiver = block.registers.peek(source).ok()?;
    let heap = block.registers.heap_read();
    let (_, fields) = heap.instance_data(receiver).ok()?;
    if heap.sequence_len(fields).ok()? > 32 {
        return None;
    }
    let read = heap.read_object(receiver).ok()?;
    let context = classes::ClassReadContext {
        heap,
        class_member_hints: block.class_hints,
        #[cfg(feature = "profiling")]
        profile: block.profile,
        #[cfg(test)]
        member_hint_hits: &block.work_probe.member_hint_hits,
    };
    let (target, property) = context
        .class_read_from(
            receiver,
            read,
            name,
            None,
            #[cfg(feature = "profiling")]
            true,
        )
        .ok()?;
    if property.is_some() {
        return None;
    }
    let cell = target.cell()?;
    let readonly = readonly_cell(heap, cell)?;
    let contract = readonly.contract;
    let value = readonly.value;
    if value.tag() == ValueTag::Object {
        return None;
    }
    let fields = heap.read_object(fields).ok()?;
    let offset =
        (0..fields.len()).find(|&index| fields.field(index).is_ok_and(|v| v.same_bits(cell)))?;
    let work = target.work() * if heap.reactive_evaluating() { 2 } else { 1 };
    Some((
        Guard {
            offset,
            contract,
            expected: value,
        },
        value,
        work,
    ))
}

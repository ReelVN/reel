//! Whitelisted literal construction is converted to a data recipe, never interpreted as script.
use super::*;
use crate::bytecode::{Const, ConstId, Descriptor, FunctionId, Opcode, TableSource, TypeId};

#[derive(Clone)]
enum Node {
    Constant(ConstId),
    Array(Vec<u32>),
    Tuple(Vec<u32>),
    Table(Vec<[u32; 2]>),
}
/// A current-code recipe proven to contain only constants and fixed containers.
pub struct DefaultPlan {
    nodes: Vec<Node>,
    checks: Vec<(u32, TypeId)>,
    result: u32,
    bytes: usize,
    charge: HeapReservation,
}
enum Collect {
    Sequence {
        dest: u32,
        node: u32,
        start: u32,
        len: u32,
        next: u32,
    },
    Table {
        dest: u32,
        node: u32,
        descriptor: u32,
        next: usize,
    },
}
pub(crate) struct Proof {
    program: crate::bytecode::VerifiedProgram,
    function: FunctionId,
    pc: usize,
    registers: BTreeMap<u32, u32>,
    plan: DefaultPlan,
    collect: Option<Collect>,
    done: bool,
}
impl Proof {
    pub(crate) fn new(
        program: &crate::bytecode::VerifiedProgram,
        usage: &crate::HeapBudgetUsage,
        function: FunctionId,
    ) -> Result<Self, SchemaError> {
        let f = &program.package().functions[function.index()];
        if !f.captures.is_empty() || !f.regions.is_empty() {
            return Err(SchemaError::Contract(
                "default captures or exception handlers are not pure data",
            ));
        }
        Ok(Self {
            program: program.clone(),
            function,
            pc: 0,
            registers: BTreeMap::new(),
            plan: DefaultPlan {
                nodes: Vec::new(),
                checks: Vec::new(),
                result: 0,
                bytes: 0,
                charge: usage.reserve(0)?,
            },
            collect: None,
            done: false,
        })
    }
    fn read(&self, reg: u32) -> Result<u32, SchemaError> {
        self.registers
            .get(&reg)
            .copied()
            .ok_or(SchemaError::Contract(
                "default reads self or a non-data input",
            ))
    }
    pub(crate) fn step(&mut self) -> Result<bool, SchemaError> {
        fn growth<T>(v: &Vec<T>) -> usize {
            if v.len() == v.capacity() {
                v.capacity().saturating_mul(2).max(4) * size_of::<T>()
            } else {
                0
            }
        }
        let container = self.collect.as_ref().map_or(0, |collect| {
            let node = match collect {
                Collect::Sequence { node, .. } | Collect::Table { node, .. } => *node,
            };
            match &self.plan.nodes[node as usize] {
                Node::Array(v) | Node::Tuple(v) => growth(v),
                Node::Table(v) => growth(v),
                _ => 0,
            }
        });
        self.plan.charge.resize(
            self.plan.charge.bytes()
                + (growth(&self.plan.nodes) + growth(&self.plan.checks) + container + 256) as u64,
        )?;
        let result = self.advance();
        let retained = self.plan.bytes + self.registers.len().max(1) * 128;
        self.plan.charge.resize(retained as u64)?;
        result
    }
    fn node(&mut self, node: Node) -> u32 {
        let old = self.plan.nodes.capacity();
        let id = self.plan.nodes.len() as u32;
        self.plan.nodes.push(node);
        self.plan.bytes += (self.plan.nodes.capacity() - old) * size_of::<Node>();
        id
    }
    fn advance(&mut self) -> Result<bool, SchemaError> {
        if self.done {
            return Ok(true);
        }
        let program = self.program.clone();
        let p = program.package();
        let f = &p.functions[self.function.index()];
        if let Some(collect) = self.collect.take() {
            match collect {
                Collect::Sequence {
                    dest,
                    node,
                    start,
                    len,
                    next,
                } => {
                    if next == len {
                        self.registers.insert(dest, node);
                        return Ok(false);
                    }
                    let value = self.read(start + next)?;
                    let (Node::Array(values) | Node::Tuple(values)) =
                        &mut self.plan.nodes[node as usize]
                    else {
                        unreachable!()
                    };
                    let old = values.capacity();
                    values.push(value);
                    self.plan.bytes += (values.capacity() - old) * size_of::<u32>();
                    self.collect = Some(Collect::Sequence {
                        dest,
                        node,
                        start,
                        len,
                        next: next + 1,
                    });
                }
                Collect::Table {
                    dest,
                    node,
                    descriptor,
                    next,
                } => {
                    let pairs = match &f.descriptors[descriptor as usize] {
                        Descriptor::Table(pairs) => pairs.get(next).copied(),
                        Descriptor::TableSources(sources) => match sources.get(next) {
                            Some(TableSource::Pair(pair)) => Some(*pair),
                            Some(TableSource::Spread(_)) => {
                                return Err(SchemaError::Contract(
                                    "default spread is not supported",
                                ));
                            }
                            None => None,
                        },
                        _ => unreachable!(),
                    };
                    let Some([a, b]) = pairs else {
                        self.registers.insert(dest, node);
                        return Ok(false);
                    };
                    let pair = [self.read(a.0)?, self.read(b.0)?];
                    let Node::Table(values) = &mut self.plan.nodes[node as usize] else {
                        unreachable!()
                    };
                    let old = values.capacity();
                    values.push(pair);
                    self.plan.bytes += (values.capacity() - old) * size_of::<[u32; 2]>();
                    self.collect = Some(Collect::Table {
                        dest,
                        node,
                        descriptor,
                        next: next + 1,
                    });
                }
            }
            return Ok(false);
        }
        let i = f
            .code
            .instruction(self.pc)
            .map_err(|_| SchemaError::Contract("default code cannot be read"))?
            .ok_or(SchemaError::Contract("default has no data result"))?;
        self.pc = f
            .code
            .next_pc(self.pc)
            .map_err(|_| SchemaError::Contract("default code cannot be read"))?
            .unwrap();
        let [a, b, c] = i.operands;
        let op = crate::bytecode::instruction_spec(i.opcode).unwrap().opcode;
        match op {
            Opcode::LoadConst => {
                let id = self.node(Node::Constant(ConstId(b)));
                self.registers.insert(a, id);
            }
            Opcode::Move => {
                let value = self.read(b)?;
                self.registers.insert(a, value);
            }
            Opcode::Clear => {
                self.registers.remove(&a);
            }
            Opcode::Clear2 => {
                self.registers.remove(&a);
                self.registers.remove(&b);
            }
            Opcode::Clear3 => {
                for reg in [a, b, c] {
                    self.registers.remove(&reg);
                }
            }
            Opcode::CheckType => {
                let old = self.plan.checks.capacity();
                self.plan.checks.push((self.read(a)?, TypeId(b)));
                self.plan.bytes += (self.plan.checks.capacity() - old) * size_of::<(u32, TypeId)>();
            }
            Opcode::MakeArray | Opcode::MakeTuple => {
                let Descriptor::Sequence(window) = f.descriptors[b as usize] else {
                    unreachable!()
                };
                let node = self.node(if op == Opcode::MakeArray {
                    Node::Array(Vec::new())
                } else {
                    Node::Tuple(Vec::new())
                });
                self.collect = Some(Collect::Sequence {
                    dest: a,
                    node,
                    start: window.start.0,
                    len: window.len,
                    next: 0,
                });
            }
            Opcode::MakeTable => {
                let node = self.node(Node::Table(Vec::new()));
                self.collect = Some(Collect::Table {
                    dest: a,
                    node,
                    descriptor: b,
                    next: 0,
                });
            }
            Opcode::Return => {
                self.plan.result = self.read(a)?;
                self.done = true;
                return Ok(true);
            }
            _ => {
                return Err(SchemaError::Contract(
                    "default requires a call, external read, write or control flow",
                ));
            }
        }
        Ok(false)
    }
    pub(super) fn finish(self) -> Result<DefaultPlan, SchemaError> {
        let Self {
            mut plan,
            registers,
            ..
        } = self;
        drop(registers);
        plan.charge.resize(plan.bytes as u64)?;
        Ok(plan)
    }
}
impl DefaultPlan {
    /// Begin a fresh default graph. Release on completion or cancellation.
    pub fn begin(&self, heap: &mut Heap, owner: RootOwner) -> Result<DefaultBuild, SchemaError> {
        let charge = heap
            .budget_usage()
            .reserve(size_of::<DefaultBuild>() as u64)?;
        let values = heap.alloc_array(&[])?;
        let root = heap.add_root(owner, RootKind::Host, values)?;
        Ok(DefaultBuild {
            root,
            node: 0,
            edge: 0,
            checks: 0,
            constant: Vec::new(),
            done: false,
            observable: false,
            charge,
        })
    }
}
struct ConstantFrame {
    id: ConstId,
    value: Value,
    next: usize,
}
/// Incremental materialization, with every intermediate value retained by an explicit root.
pub struct DefaultBuild {
    root: RootToken,
    node: usize,
    edge: usize,
    checks: usize,
    constant: Vec<ConstantFrame>,
    done: bool,
    observable: bool,
    charge: HeapReservation,
}
impl DefaultBuild {
    /// Preserve model semantics for the fresh containers produced by this recipe.
    pub fn observe_containers(&mut self) {
        self.observable = true;
    }
    fn push_constant(&mut self, heap: &Heap, frame: ConstantFrame) -> Result<(), SchemaError> {
        if self.constant.len() == self.constant.capacity() {
            let capacity = (self.constant.capacity() * 2).max(4);
            let mut charge = heap
                .budget_usage()
                .reserve((size_of::<Self>() + capacity * size_of::<ConstantFrame>()) as u64)?;
            let mut data = Vec::new();
            data.try_reserve_exact(capacity)
                .map_err(|_| SchemaError::Allocation)?;
            charge.resize(
                (size_of::<Self>() + data.capacity() * size_of::<ConstantFrame>()) as u64,
            )?;
            data.append(&mut self.constant);
            let old = std::mem::replace(&mut self.constant, data);
            drop(old);
            self.charge = charge;
        }
        self.constant.push(frame);
        Ok(())
    }
    fn constant(
        &mut self,
        heap: &mut Heap,
        id: ConstId,
        values: Value,
    ) -> Result<Option<Value>, SchemaError> {
        let code = heap
            .code
            .as_ref()
            .ok_or(SchemaError::Contract("State defaults require current code"))?
            .clone();
        if self.constant.is_empty() {
            let value = match &code.package().constants[id.index()] {
                Const::Null => Value::NULL,
                Const::Bool(v) => Value::bool(*v),
                Const::Int(v) => Value::int(*v),
                Const::FloatBits(v) => Value::float(f64::from_bits(*v)),
                Const::Utf8(_) | Const::Text { .. } => {
                    heap.alloc_constant_bytes(crate::ObjectKind::String, code.clone(), id)?
                }
                Const::Bytes(_) => {
                    heap.alloc_constant_bytes(crate::ObjectKind::Bytes, code.clone(), id)?
                }
                Const::Tuple(_) => heap.alloc_array(&[])?,
            };
            heap.array_push(values, value)?;
            if !matches!(code.package().constants[id.index()], Const::Tuple(_)) {
                return Ok(Some(value));
            }
            self.push_constant(heap, ConstantFrame { id, value, next: 0 })?;
            return Ok(None);
        }
        let frame = self.constant.last_mut().unwrap();
        let Const::Tuple(children) = &code.package().constants[frame.id.index()] else {
            unreachable!()
        };
        if frame.next == children.len() {
            let value = frame.value;
            heap.seal_tuple(value)?;
            if self.observable {
                heap.observe_restored_data(value)?;
            }
            self.constant.pop();
            if self.constant.last().is_some() {
                return Ok(None);
            }
            return Ok(Some(value));
        }
        let id = children[frame.next];
        frame.next += 1;
        let parent = frame.value;
        let value = match &code.package().constants[id.index()] {
            Const::Null => Value::NULL,
            Const::Bool(v) => Value::bool(*v),
            Const::Int(v) => Value::int(*v),
            Const::FloatBits(v) => Value::float(f64::from_bits(*v)),
            Const::Utf8(_) | Const::Text { .. } => {
                heap.alloc_constant_bytes(crate::ObjectKind::String, code.clone(), id)?
            }
            Const::Bytes(_) => {
                heap.alloc_constant_bytes(crate::ObjectKind::Bytes, code.clone(), id)?
            }
            Const::Tuple(_) => {
                let value = heap.alloc_array(&[])?;
                heap.array_push(parent, value)?;
                self.push_constant(heap, ConstantFrame { id, value, next: 0 })?;
                return Ok(None);
            }
        };
        heap.array_push(parent, value)?;
        Ok(None)
    }
    /// Each unit creates a scalar/container or connects one edge. Type checks use
    /// the current verified program's already bounded structural validator.
    pub fn step(
        &mut self,
        heap: &mut Heap,
        plan: &DefaultPlan,
        work: u32,
    ) -> Result<bool, SchemaError> {
        let values = heap.root_value(self.root)?;
        for _ in 0..work {
            if self.node == plan.nodes.len() {
                if self.checks < plan.checks.len() {
                    let (node, ty) = plan.checks[self.checks];
                    let code = heap.code.as_ref().unwrap();
                    if !code.checks.accepts(
                        code.package(),
                        heap,
                        ty,
                        heap.sequence_get(values, node)?,
                    )? {
                        return Err(SchemaError::Contract(
                            "default violates its current type contract",
                        ));
                    }
                    self.checks += 1;
                    continue;
                }
                self.done = true;
                return Ok(true);
            }
            let node = &plan.nodes[self.node];
            if let Node::Constant(id) = node {
                if self.constant(heap, *id, values)?.is_some() {
                    self.node += 1;
                }
                continue;
            }
            if heap.sequence_len(values)? as usize == self.node {
                let value = if matches!(node, Node::Table(_)) {
                    heap.alloc_table()?
                } else {
                    heap.alloc_array(&[])?
                };
                heap.array_push(values, value)?;
                continue;
            }
            let value = heap.sequence_get(values, self.node as u32)?;
            match node {
                Node::Array(items) | Node::Tuple(items) if self.edge < items.len() => {
                    heap.array_push(value, heap.sequence_get(values, items[self.edge])?)?;
                    self.edge += 1;
                }
                Node::Table(items) if self.edge < items.len() => {
                    let [a, b] = items[self.edge];
                    heap.table_store(
                        value,
                        self.edge as u32,
                        heap.sequence_get(values, a)?,
                        heap.sequence_get(values, b)?,
                    )?;
                    self.edge += 1;
                }
                _ => {
                    if matches!(node, Node::Tuple(_)) {
                        heap.seal_tuple(value)?;
                    }
                    if self.observable {
                        heap.observe_restored_data(value)?;
                    }
                    self.node += 1;
                    self.edge = 0;
                }
            }
        }
        Ok(false)
    }
    pub fn value(&self, heap: &Heap, plan: &DefaultPlan) -> Result<Value, SchemaError> {
        if !self.done {
            return Err(SchemaError::Contract("default graph is incomplete"));
        }
        Ok(heap.sequence_get(heap.root_value(self.root)?, plan.result)?)
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), SchemaError> {
        heap.release_root(self.root)?;
        Ok(())
    }
}

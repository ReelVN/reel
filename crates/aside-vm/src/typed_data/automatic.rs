//! Build one rooted registry from the single current-code State declaration table.
use super::*;
use crate::{bytecode as bc, executor::Vm, resume::VerifiedResumeTable};

/// Incremental State identity and current-class registration after module initialization.
/// Finish or cancel explicitly; cancellation releases its class root and native scratch.
pub struct RegistryInitCursor {
    table: VerifiedResumeTable,
    registry: Registry,
    phase: u8,
    class: usize,
    field: usize,
    nominal: usize,
    active: Option<u32>,
    done: bool,
}
fn tree_bytes<K, V>(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (len / 5 + 1) * (128 + 11 * (size_of::<K>() + size_of::<V>()))
    }
}
impl RegistryInitCursor {
    /// Retain the verified table and create a fixed empty registry root.
    pub fn new(
        vm: &mut Vm,
        table: &VerifiedResumeTable,
        owner: RootOwner,
    ) -> Result<Self, SchemaError> {
        if vm.program().snapshot_identity() != table.program.snapshot_identity() {
            return Err(SchemaError::Contract(
                "State declarations belong to another current program",
            ));
        }
        let mut registry = Registry::new(vm.heap_mut(), owner, Identities::default())?;
        if let Err(error) = registry.grow(size_of::<Self>()) {
            registry.release(vm.heap_mut())?;
            return Err(error);
        }
        Ok(Self {
            table: table.clone(),
            registry,
            phase: 0,
            class: 0,
            field: 0,
            nominal: 0,
            active: None,
            done: false,
        })
    }
    /// Each unit examines a nominal type, registers a State/field identity, or
    /// advances existing class/default work. Native identities come only from
    /// the current heap's admitted host types.
    pub fn step(&mut self, vm: &mut Vm, work: u32) -> Result<bool, SchemaError> {
        if self.done {
            return Ok(true);
        }
        for _ in 0..work {
            if self.phase == 2 {
                let p = vm.program().package();
                if self.nominal == p.types.len() {
                    self.phase = 3;
                    continue;
                }
                let id = bc::TypeId(self.nominal as u32);
                self.nominal += 1;
                if let bc::TypeDef::Named { module, name, .. } = p.types[id.index()]
                    && vm
                        .heap()
                        .code
                        .as_ref()
                        .unwrap()
                        .checks
                        .host_types
                        .contains(id)
                {
                    let module = &p.symbols[p.modules[module.index()].path.index()];
                    let name = &p.symbols[name.index()];
                    let count = self.registry.identities.logical.len();
                    self.registry.grow(
                        tree_bytes::<TypeKey, String>(count + 1)
                            - tree_bytes::<TypeKey, String>(count)
                            + 2 * (module.len() + name.len())
                            + 2,
                    )?;
                    let key = TypeKey {
                        module: module.clone(),
                        name: name.clone(),
                    };
                    if self.registry.identities.types.contains_key(&key) {
                        return Err(SchemaError::State {
                            key,
                            reason: "script State conflicts with an admitted native type",
                        });
                    }
                    let canonical = format!("{module}::{name}");
                    let requested = 2 * (module.len() + name.len()) + 2;
                    let actual = key.module.capacity() + key.name.capacity() + canonical.capacity();
                    if actual > requested {
                        self.registry.grow(actual - requested)?;
                    }
                    self.registry.identities.logical.insert(key, canonical);
                }
                continue;
            }
            let states = &self.table.table().states;
            if self.class == states.len() {
                if self.phase == 3 {
                    self.done = true;
                    return Ok(true);
                }
                self.phase += 1;
                self.class = 0;
                self.field = 0;
                continue;
            }
            let state = &states[self.class];
            let p = vm.program().package();
            let bc::TypeDef::Named { module, name, .. } = p.types[state.class.nominal.index()]
            else {
                unreachable!()
            };
            let module = &p.symbols[p.modules[module.index()].path.index()];
            let name = &p.symbols[name.index()];
            match self.phase {
                0 => {
                    let count = self.registry.identities.types.len();
                    let bytes = tree_bytes::<TypeKey, TypeKey>(count + 1)
                        - tree_bytes::<TypeKey, TypeKey>(count)
                        + module.len()
                        + name.len()
                        + state.key.module.len()
                        + state.key.name.len();
                    self.registry.grow(bytes)?;
                    self.registry.identities.types.insert(
                        TypeKey {
                            module: module.clone(),
                            name: name.clone(),
                        },
                        state.key.clone(),
                    );
                    self.class += 1;
                }
                1 => {
                    if self.field == state.fields.len() {
                        self.class += 1;
                        self.field = 0;
                        continue;
                    }
                    let field = &state.fields[self.field];
                    let field_name = &p.symbols[field.field.index()];
                    let count = self.registry.identities.fields.len();
                    let bytes = tree_bytes::<FieldKey, FieldKey>(count + 1)
                        - tree_bytes::<FieldKey, FieldKey>(count)
                        + module.len()
                        + name.len()
                        + field_name.len()
                        + state.key.module.len()
                        + state.key.name.len()
                        + field.key.len();
                    self.registry.grow(bytes)?;
                    self.registry.identities.fields.insert(
                        FieldKey {
                            owner: TypeKey {
                                module: module.clone(),
                                name: name.clone(),
                            },
                            name: field_name.clone(),
                        },
                        FieldKey {
                            owner: state.key.clone(),
                            name: field.key.clone(),
                        },
                    );
                    self.field += 1;
                }
                _ => {
                    if let Some(class) = self.active {
                        if self.registry.step_class(vm.heap(), class, 1)? {
                            self.active = None;
                            self.class += 1;
                        }
                    } else {
                        let class = vm.state_class(&self.table, self.class as u32)?;
                        self.active = Some(self.registry.begin_class(vm.heap_mut(), class)?);
                    }
                }
            }
        }
        Ok(false)
    }
    /// Transfer the completed registry, or release an incomplete candidate.
    pub fn finish(self, heap: &mut Heap) -> Result<Registry, SchemaError> {
        if !self.done {
            self.registry.release(heap)?;
            return Err(SchemaError::Contract(
                "automatic State registry is incomplete",
            ));
        }
        Ok(self.registry)
    }
    pub fn cancel(self, heap: &mut Heap) -> Result<(), SchemaError> {
        self.registry.release(heap)
    }
}

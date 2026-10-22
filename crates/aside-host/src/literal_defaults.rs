//! Seed-only interning of registered constant defaults with exact prefix types.
use aside_vm::bytecode as bc;
use std::collections::BTreeMap;

type Prefix = Vec<(bc::SymbolId, bc::TypeId, bool)>;
type Key = (bc::ConstId, bc::TypeId, Prefix);

#[derive(Default)]
pub(crate) struct LiteralDefaults {
    functions: BTreeMap<Key, bc::FunctionId>,
    name: Option<bc::SymbolId>,
}
impl LiteralDefaults {
    pub(crate) fn intern(
        &mut self,
        package: &mut bc::BytecodePackage,
        prefix: &[bc::Parameter],
        result: bc::TypeId,
        value: &bc::Const,
    ) -> bc::FunctionId {
        let constant = if let Some(index) = package
            .constants
            .iter()
            .position(|constant| constant == value)
        {
            bc::ConstId(index as u32)
        } else {
            let index = bc::ConstId(package.constants.len() as u32);
            package.constants.push(value.clone());
            index
        };
        let key = (
            constant,
            result,
            prefix.iter().map(|p| (p.name, p.ty, p.mutable)).collect(),
        );
        if let Some(&function) = self.functions.get(&key) {
            return function;
        }
        let name = *self.name.get_or_insert_with(|| {
            let name = bc::SymbolId(package.symbols.len() as u32);
            package.symbols.push("$literal_default".into());
            name
        });
        let signature = bc::SignatureId(package.signatures.len() as u32);
        package.signatures.push(bc::Signature {
            parameters: prefix
                .iter()
                .cloned()
                .map(|mut p| {
                    p.default = None;
                    p
                })
                .collect(),
            rest: None,
            return_type: result,
            effects: bc::Effects(0),
            property_bag_tail: None,
        });
        let register = prefix.len() as u32;
        let function = bc::FunctionId(package.functions.len() as u32);
        package.functions.push(bc::FunctionDef {
            module: bc::ModuleId(0),
            name,
            kind: bc::FunctionKind::Plain,
            signature,
            context: bc::Context::NoWait,
            captures: vec![],
            register_count: register + 1,
            bindings: prefix
                .iter()
                .enumerate()
                .map(|(index, p)| bc::BindingDef {
                    name: p.name,
                    ty: p.ty,
                    mutable: p.mutable,
                    storage: bc::BindingStorage::Value(bc::Reg(index as u32)),
                    initialization: bc::BindingInitialization::Parameter(index as u32),
                })
                .collect(),
            descriptors: vec![],
            regions: vec![],
            source_map: vec![],
            code: vec![
                bc::Instruction::new(bc::Opcode::LoadConst, register, constant.0, 0),
                bc::Instruction::new(bc::Opcode::Return, register, 0, 0),
            ],
        });
        self.functions.insert(key, function);
        function
    }
}

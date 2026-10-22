//! Precise author types and read-only declarations shared by embedding tools.
use crate::{NativeContract, PropertyDocumentation};
use aside_compiler::{
    TextRange, TextSize,
    semantic::{
        ExternalMember, ExternalSymbol, ModuleInterface, NamespaceRef, SymbolReference, TypeHint,
    },
};
use std::collections::BTreeMap;

/// A source-visible default value or a parameter handled only when supplied.
/// Native ABI defaults remain on the independently registered contract.
#[derive(Clone, Debug)]
pub enum ParameterDefault {
    /// The literal value used when the author omits this parameter.
    Value(aside_vm::bytecode::Const),
    /// The host distinguishes absence from every explicitly supplied value.
    Omitted,
}
impl ParameterDefault {
    /// Return the literal author default, when omission has a source value.
    pub fn value(&self) -> Option<&aside_vm::bytecode::Const> {
        match self {
            Self::Value(value) => Some(value),
            Self::Omitted => None,
        }
    }
}

/// A declaration retains its compiler identity, documentation and real defaults.
#[derive(Clone, Debug)]
pub struct Declaration {
    pub symbol: ExternalSymbol,
    pub documentation: String,
    pub parameter_defaults: BTreeMap<String, ParameterDefault>,
    pub property_default: Option<aside_vm::bytecode::Const>,
    /// Parameter names expanded from the implementation's registered property schema.
    pub property_parameters: Vec<String>,
}
impl Declaration {
    /// Describe a registered module using the compiler's namespace table semantics.
    pub fn namespace(module: &str, version: &str, documentation: &str) -> Self {
        let mut symbol =
            ExternalSymbol::value(module.rsplit("::").next().unwrap(), TypeHint::Table)
                .with_reference(SymbolReference {
                    module: module.into(),
                    version: version.into(),
                    export: None,
                });
        symbol.namespace = Some(NamespaceRef {
            module: module.into(),
            version: version.into(),
        });
        Self {
            symbol,
            documentation: documentation.into(),
            parameter_defaults: BTreeMap::new(),
            property_default: None,
            property_parameters: Vec::new(),
        }
    }

    /// Describe a native without copying its parameter names, defaults or rest policy.
    pub fn native(contract: &NativeContract, documentation: &str) -> Self {
        Self {
            symbol: contract.external(),
            documentation: documentation.into(),
            parameter_defaults: contract
                .parameters
                .iter()
                .filter_map(|p| {
                    p.default
                        .as_ref()
                        .map(|value| (p.name.clone(), ParameterDefault::Value(value.clone())))
                })
                .collect(),
            property_default: None,
            property_parameters: Vec::new(),
        }
    }
    /// Override only known source types on the existing registered signature.
    pub fn types(mut self, result: TypeHint, parameters: &[(&str, TypeHint)]) -> Self {
        let signature = self.symbol.signature.as_mut().expect("native signature");
        signature.result = result;
        for (name, ty) in parameters {
            signature
                .parameters
                .iter_mut()
                .find(|p| p.name == *name)
                .expect("source metadata names a registered parameter")
                .ty = ty.clone();
        }
        self
    }
    /// Expose the actual native as a bound method with its receiver already supplied.
    pub fn bound(mut self, name: &str) -> Self {
        let signature = self
            .symbol
            .signature
            .as_mut()
            .expect("bound native signature");
        assert!(!signature.parameters.is_empty(), "bound native receiver");
        let receiver = signature.parameters.remove(0);
        if signature.result_parameter.as_deref() == Some(&receiver.name) {
            signature.result = receiver.ty;
            signature.result_parameter = None;
        }
        self.parameter_defaults.remove(&receiver.name);
        self.symbol.name = name.into();
        self
    }

    /// Retain the actual argument's type when this native returns that same value.
    pub fn returns_parameter(&mut self, name: &str) {
        let signature = self.symbol.signature.as_mut().expect("native signature");
        assert!(
            signature
                .parameters
                .iter()
                .any(|parameter| parameter.name == name),
            "result parameter names an actual parameter"
        );
        signature.result_parameter = Some(name.into());
    }

    /// Describe optional parameters whose omitted ABI values are ignored by the
    /// host. Their source types still govern every explicitly supplied value.
    pub fn supplied_only(&mut self, names: &[&str]) {
        let signature = self.symbol.signature.as_ref().expect("native signature");
        for name in names {
            assert!(
                signature
                    .parameters
                    .iter()
                    .any(|parameter| parameter.name == *name && parameter.default),
                "supplied-only parameter is an actual optional native parameter"
            );
            self.parameter_defaults
                .insert((*name).into(), ParameterDefault::Omitted);
        }
    }

    /// Refine constant-dependent results using the native's original defaults.
    pub fn result_cases(&mut self, cases: &[(&str, aside_vm::bytecode::Const, TypeHint)]) {
        let signature = self.symbol.signature.as_mut().expect("native signature");
        signature.result_cases = cases
            .iter()
            .map(|(parameter, equals, result)| {
                assert!(
                    signature.parameters.iter().any(|p| p.name == *parameter),
                    "result case names an actual parameter"
                );
                aside_compiler::semantic::ExternalResultCase {
                    parameter: (*parameter).into(),
                    equals: equals.clone(),
                    result: result.clone(),
                    when_omitted: self
                        .parameter_defaults
                        .get(*parameter)
                        .is_some_and(|value| value.value() == Some(equals)),
                }
            })
            .collect();
    }
}

/// A named host member uses one read type and an optional independent write type.
#[derive(Clone, Debug)]
pub struct Member {
    pub declaration: Declaration,
    pub write_type: Option<TypeHint>,
}
impl Member {
    /// Convert the real property schema, including source docs and registered default.
    pub fn property(owner: &str, doc: PropertyDocumentation) -> Self {
        let property = doc.property;
        let ty = property.source_type();
        Self::field(
            owner,
            property.name,
            ty.clone(),
            property.writable.then_some(ty),
            doc.markdown,
            Some((property.default)()),
        )
    }
    /// Describe a published field beside the host implementation that owns it.
    pub fn field(
        owner: &str,
        name: &str,
        ty: TypeHint,
        write_type: Option<TypeHint>,
        documentation: &str,
        default: Option<aside_vm::bytecode::Const>,
    ) -> Self {
        let mut symbol = ExternalSymbol::dynamic(name);
        symbol.ty = ty;
        symbol.reference = Some(std::sync::Arc::new(SymbolReference {
            module: owner.into(),
            version: "1".into(),
            export: Some(name.into()),
        }));
        Self {
            declaration: Declaration {
                symbol,
                documentation: documentation.into(),
                parameter_defaults: BTreeMap::new(),
                property_default: default,
                property_parameters: Vec::new(),
            },
            write_type,
        }
    }
    /// Bound native methods keep the native's canonical identity for navigation.
    pub fn method(declaration: Declaration) -> Self {
        Self {
            declaration,
            write_type: None,
        }
    }
    /// Produce the exact compiler member, without documentation or editor dependencies.
    pub fn external(&self) -> ExternalMember {
        ExternalMember {
            symbol: self.declaration.symbol.clone(),
            write_type: self.write_type.clone(),
        }
    }
}

/// A host type's complete visible members, including inherited members.
#[derive(Clone, Debug)]
pub struct NamedType {
    pub name: String,
    pub documentation: String,
    pub members: Vec<Member>,
    pub version: u64,
    pub bases: Vec<String>,
    /// Signature of the host value's real call adapter, with the receiver bound.
    pub call: Option<Declaration>,
    /// Result produced by the host value's real await adapter.
    pub await_result: Option<TypeHint>,
}
impl NamedType {
    /// The type name can be imported and used in source annotations.
    pub fn symbol(&self) -> ExternalSymbol {
        let (module, name) = self.name.rsplit_once("::").expect("canonical host type");
        let mut symbol = ExternalSymbol::dynamic(name).with_reference(SymbolReference {
            module: module.into(),
            version: self.version.to_string(),
            export: Some(name.into()),
        });
        symbol.ty = named(&self.name);
        symbol.type_only = true;
        symbol
    }
    /// Use the canonical module/export identity as the type name.
    pub fn new(name: &str, documentation: &str, members: Vec<Member>) -> Self {
        Self {
            name: name.into(),
            documentation: documentation.into(),
            members,
            version: 1,
            bases: Vec::new(),
            call: None,
            await_result: None,
        }
    }

    /// Inherit the real host base while preserving each member's declaration identity.
    pub fn extends(mut self, base: &NamedType) -> Self {
        self.bases.push(base.name.clone());
        if self.call.is_none() {
            self.call = base.call.clone();
        }
        if self.await_result.is_none() {
            self.await_result = base.await_result.clone();
        }
        let mut members = base.members.clone();
        for member in self.members {
            if let Some(existing) = members
                .iter_mut()
                .find(|existing| existing.declaration.symbol.name == member.declaration.symbol.name)
            {
                *existing = member;
            } else {
                members.push(member);
            }
        }
        self.members = members;
        self
    }

    /// Produce the trusted runtime registration from the same source declaration.
    pub fn registration(&self) -> aside_vm::bytecode::HostTypeSupport {
        aside_vm::bytecode::HostTypeSupport {
            name: self.name.clone(),
            version: self.version,
            bases: self.bases.clone(),
            methods: self
                .members
                .iter()
                .filter_map(|member| {
                    let symbol = &member.declaration.symbol;
                    symbol.signature.as_ref()?;
                    let reference = symbol.reference.as_ref()?;
                    Some(aside_vm::HostMethodSupport {
                        name: symbol.name.clone(),
                        native: format!("{}::{}", reference.module, reference.export.as_deref()?),
                        abi_version: reference.version.parse().expect("native ABI version"),
                    })
                })
                .collect(),
        }
    }
}

/// Source semantics and generated declarations supplied by an embedding host.
#[derive(Default, Clone, Debug)]
pub struct Api {
    pub globals: Vec<ExternalSymbol>,
    pub modules: Vec<ModuleInterface>,
    pub types: Vec<NamedType>,
    pub functions: Vec<Declaration>,
    /// Module descriptions retain canonical namespace identities and English docs.
    pub namespaces: Vec<Declaration>,
}
/// One generated Aside declaration document, intended for display rather than execution.
#[derive(Clone, Debug)]
pub struct Document {
    pub uri: String,
    pub text: String,
}
/// A canonical declaration's exact range in the generated source.
#[derive(Clone, Debug)]
pub struct Location {
    pub declaration: Declaration,
    pub uri: String,
    pub range: TextRange,
}
/// Generated documents and their original declaration identities.
#[derive(Clone, Debug, Default)]
pub struct Documents {
    pub documents: Vec<Document>,
    pub locations: Vec<Location>,
}

impl Api {
    /// Resolve a member from the same complete schema used by editor clients.
    pub fn member(&self, type_name: &str, member: &str) -> Option<ExternalMember> {
        self.types
            .iter()
            .find(|ty| ty.name == type_name)?
            .members
            .iter()
            .find(|candidate| candidate.declaration.symbol.name == member)
            .map(Member::external)
    }
    /// Query registered nominal ancestry, leaving unknown types to the caller.
    pub fn is_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        if !self.types.iter().any(|ty| ty.name == actual)
            || !self.types.iter().any(|ty| ty.name == expected)
        {
            return None;
        }
        let mut pending = vec![actual];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(name) = pending.pop() {
            if name == expected {
                return Some(true);
            }
            if seen.insert(name) {
                pending.extend(
                    self.types
                        .iter()
                        .find(|ty| ty.name == name)
                        .expect("registered base type")
                        .bases
                        .iter()
                        .map(String::as_str),
                );
            }
        }
        Some(false)
    }
    /// Access one actual registered native by its canonical identity.
    pub fn native(&self, symbol: &str) -> &Declaration {
        self.functions
            .iter()
            .find(|declaration| canonical(&declaration.symbol) == symbol)
            .expect("source schema names an actual registered native")
    }
    /// Mutate source metadata for one registered native without replacing its identity.
    pub fn native_mut(&mut self, symbol: &str) -> &mut Declaration {
        self.functions
            .iter_mut()
            .find(|declaration| canonical(&declaration.symbol) == symbol)
            .expect("source schema names an actual registered native")
    }
    /// Refine an existing native's source types while preserving its parameter contract.
    pub fn refine(&mut self, symbol: &str, result: TypeHint, parameters: &[(&str, TypeHint)]) {
        let declaration = self
            .functions
            .iter_mut()
            .find(|declaration| canonical(&declaration.symbol) == symbol)
            .expect("source schema names an actual registered native");
        let signature = declaration
            .symbol
            .signature
            .as_mut()
            .expect("native signature");
        signature.result = result;
        for (name, ty) in parameters {
            signature
                .parameters
                .iter_mut()
                .find(|p| p.name == *name)
                .expect("source metadata names an actual parameter")
                .ty = ty.clone();
        }
    }
    /// Read the host's nominal types without keeping a second registration table.
    pub fn registrations(&self) -> Vec<aside_vm::bytecode::HostTypeSupport> {
        self.types.iter().map(NamedType::registration).collect()
    }
    /// Generate module roots, complete classes and native signatures from the same schemas.
    pub fn documents(&self, authority: &str) -> Documents {
        let mut result = Documents::default();
        for ty in &self.types {
            let (module, name) = ty.name.rsplit_once("::").expect("canonical host type");
            let uri = format!(
                "aside-native://{authority}/types/{}.aside",
                ty.name.replace("::", "/")
            );
            let mut text = String::new();
            append_docs(&mut text, "", &ty.documentation);
            let start = text.len();
            text.push_str(&format!("pub class {name}"));
            if let Some(base) = ty.bases.first() {
                text.push_str(": ");
                text.push_str(base.rsplit("::").next().unwrap());
            }
            text.push_str(" {\n");
            let symbol = ExternalSymbol::dynamic(name).with_reference(SymbolReference {
                module: module.into(),
                version: "1".into(),
                export: Some(name.into()),
            });
            result.locations.push(Location {
                declaration: Declaration {
                    symbol,
                    documentation: ty.documentation.clone(),
                    parameter_defaults: BTreeMap::new(),
                    property_default: None,
                    property_parameters: Vec::new(),
                },
                uri: uri.clone(),
                range: range(start + 10, name.len()),
            });
            for member in &ty.members {
                let inherited = ty.bases.iter().any(|base| {
                    self.types
                        .iter()
                        .find(|candidate| candidate.name == *base)
                        .expect("registered base type")
                        .members
                        .iter()
                        .any(|base_member| {
                            base_member.declaration.symbol.reference
                                == member.declaration.symbol.reference
                                && base_member.declaration.symbol.signature
                                    == member.declaration.symbol.signature
                                && base_member.declaration.symbol.ty == member.declaration.symbol.ty
                                && base_member.write_type == member.write_type
                        })
                });
                if inherited {
                    continue;
                }
                append_declaration(
                    &mut text,
                    "    ",
                    &member.declaration,
                    member.write_type.is_some(),
                    &uri,
                    &mut result.locations,
                );
            }
            text.push_str("}\n");
            result.documents.push(Document { uri, text });
        }
        let mut modules: BTreeMap<&str, Vec<&Declaration>> = BTreeMap::new();
        for declaration in &self.functions {
            let reference = declaration
                .symbol
                .reference
                .as_ref()
                .expect("registered declaration identity");
            modules
                .entry(&reference.module)
                .or_default()
                .push(declaration);
        }
        for module in &self.modules {
            modules.entry(&module.canonical_path).or_default();
        }
        for (module, declarations) in modules {
            let uri = format!(
                "aside-native://{authority}/modules/{}.aside",
                module.replace("::", "/")
            );
            let mut text = String::new();
            let registered = self
                .modules
                .iter()
                .find(|entry| entry.canonical_path == module);
            let mut declaration = self
                .namespaces
                .iter()
                .find(|declaration| {
                    declaration
                        .symbol
                        .namespace
                        .as_ref()
                        .is_some_and(|namespace| namespace.module == module)
                })
                .cloned()
                .unwrap_or_else(|| {
                    Declaration::namespace(
                        module,
                        registered.map_or("1", |module| module.version.as_str()),
                        "",
                    )
                });
            if let Some(registered) = registered {
                let names = registered
                    .exports
                    .iter()
                    .map(|symbol| symbol.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if !names.is_empty() {
                    if !declaration.documentation.is_empty() {
                        declaration.documentation.push_str("\n\n");
                    }
                    declaration
                        .documentation
                        .push_str(&format!("Exports: {names}."));
                }
            }
            append_docs(&mut text, "", &declaration.documentation);
            text.push_str("use ");
            let namespace_start = text.len();
            text.push_str(module);
            let name = &declaration.symbol.name;
            let start = namespace_start + module.len() - name.len();
            text.push_str(";\n\n");
            result.locations.push(Location {
                range: range(start, name.len()),
                declaration,
                uri: uri.clone(),
            });
            for declaration in declarations {
                append_declaration(
                    &mut text,
                    "",
                    declaration,
                    false,
                    &uri,
                    &mut result.locations,
                );
            }
            result.documents.push(Document { uri, text });
        }
        result
    }
}
/// Read a declaration's original canonical module/export identity.
pub fn canonical(symbol: &ExternalSymbol) -> String {
    let reference = symbol.reference.as_ref().expect("registered identity");
    match &reference.export {
        Some(export) => format!("{}::{export}", reference.module),
        None => reference.module.clone(),
    }
}
fn range(start: usize, len: usize) -> TextRange {
    TextRange::new(
        TextSize::try_from(start).unwrap(),
        TextSize::try_from(start + len).unwrap(),
    )
}
fn append_docs(text: &mut String, indent: &str, docs: &str) {
    for line in docs.trim().lines() {
        text.push_str(indent);
        text.push_str("/// ");
        text.push_str(line);
        text.push('\n');
    }
}
fn append_declaration(
    text: &mut String,
    indent: &str,
    declaration: &Declaration,
    writable: bool,
    uri: &str,
    locations: &mut Vec<Location>,
) {
    append_docs(text, indent, &declaration.documentation);
    text.push_str(indent);
    text.push_str("pub ");
    let symbol = &declaration.symbol;
    if let Some(signature) = &symbol.signature {
        text.push_str("fn ");
        let start = text.len();
        text.push_str(&symbol.name);
        let span = range(start, symbol.name.len());
        text.push('(');
        for (index, p) in signature.parameters.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            if p.rest {
                text.push_str("...");
            }
            text.push_str(&p.name);
            let default = declaration.parameter_defaults.get(&p.name);
            if matches!(default, Some(ParameterDefault::Omitted))
                || (default.is_none() && p.default)
            {
                text.push('?');
            }
            text.push_str(": ");
            text.push_str(&type_name(&p.ty));
            if let Some(ParameterDefault::Value(default)) = default {
                text.push_str(" = ");
                text.push_str(&constant(default));
            }
        }
        text.push_str(") -> ");
        text.push_str(&type_name(&signature.result));
        text.push_str(";\n");
        locations.push(Location {
            declaration: declaration.clone(),
            uri: uri.into(),
            range: span,
        });
    } else {
        text.push_str(if writable { "let mut " } else { "let " });
        let start = text.len();
        text.push_str(&symbol.name);
        let span = range(start, symbol.name.len());
        text.push_str(": ");
        text.push_str(&type_name(&symbol.ty));
        if let Some(default) = &declaration.property_default {
            text.push_str(" = ");
            text.push_str(&constant(default));
        }
        text.push_str(";\n");
        locations.push(Location {
            declaration: declaration.clone(),
            uri: uri.into(),
            range: span,
        });
    }
}
/// Display semantic types with Aside's declaration syntax.
pub fn type_name(ty: &TypeHint) -> String {
    ty.to_string()
}
/// Preserve literal defaults from the registered contract.
pub fn constant(value: &aside_vm::bytecode::Const) -> String {
    use aside_vm::bytecode::Const;
    match value {
        Const::Null => "null".into(),
        Const::Bool(value) => value.to_string(),
        Const::Int(value) => value.to_string(),
        Const::FloatBits(value) => format!("{:?}", f64::from_bits(*value)),
        Const::Utf8(value) => format!("{value:?}"),
        _ => panic!("native default must be an author literal"),
    }
}
/// The exact numeric values accepted by common host property validators.
pub fn number() -> TypeHint {
    TypeHint::Union(vec![TypeHint::Int, TypeHint::Float])
}
/// A nullable source type retains the original non-null member information.
pub fn nullable(ty: TypeHint) -> TypeHint {
    TypeHint::Union(vec![ty, TypeHint::Null])
}
/// A canonical named type supplied by the embedding host.
pub fn named(name: &str) -> TypeHint {
    TypeHint::Named(name.into())
}

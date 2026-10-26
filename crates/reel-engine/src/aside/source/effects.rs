//! Compile statically resolved pure Aside functions into portable drawing expressions.
use aside_compiler::{
    SyntaxKind as K, TextRange,
    hir::{self as h, Form, HirKind as H},
};
use reel_ui::stage::effect::{
    EffectBinary as Binary, EffectInput, EffectNode, EffectNodeId as Id, EffectOp as Op,
    EffectParameter, EffectProgram, EffectType as Ty, EffectUnary as Unary,
    EffectValue as Constant,
};
use std::{collections::HashMap, sync::Arc};

mod collect;
mod lower;
pub use collect::collect_effects;
pub(super) use collect::{bind_project, bind_single, collect_registered};
mod products;
pub use products::{
    EffectDefinition, EffectDefinitionId, EffectLoadLimits, EffectLocation, EffectOrigin,
    EffectProductError, EffectProducts, decode_definition, encode_definition,
};
#[cfg(test)]
mod tests;

/// A function identity from the bound module set, never an unbound source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectFunction {
    pub module: usize,
    pub function: h::FunctionId,
}

/// Source metadata is separate from shared GPU structure and live parameter values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectSource {
    pub module: usize,
    pub span: TextRange,
}

/// A compile diagnostic at the original expression or parameter declaration.
#[derive(Clone, Debug)]
pub struct EffectDiagnostic {
    pub module: String,
    pub span: TextRange,
    pub message: String,
}
impl std::fmt::Display for EffectDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{:?}: {}", self.module, self.span, self.message)
    }
}
impl std::error::Error for EffectDiagnostic {}

/// Bounds for source expansion, independent of a renderer's target resource limits.
#[derive(Clone, Copy, Debug)]
pub struct EffectCompileLimits {
    pub nodes: usize,
    pub work: usize,
    pub call_depth: usize,
}
impl Default for EffectCompileLimits {
    fn default() -> Self {
        Self {
            nodes: 16_384,
            work: 1_000_000,
            call_depth: 64,
        }
    }
}

/// Source compilation product consumed by package preparation and editor tooling.
#[derive(Debug)]
pub struct CompiledEffect {
    pub entry: EffectFunction,
    pub program: Arc<EffectProgram>,
    /// One location per expression, indexed by EffectNodeId.
    pub node_sources: Vec<EffectSource>,
    pub source_files: Vec<Arc<str>>,
}

/// Resolve a bound local function or canonical imported export to its exact HIR id.
/// Module aliases and spelling do not participate in function identity.
pub fn resolve_effect_function(
    modules: &[h::AnalyzedModule],
    module: usize,
    node: h::NodeId,
) -> Option<EffectFunction> {
    let current = modules.get(module)?;
    if let Some(reference) = current.canonical_reference(node) {
        let (module, target) = modules.iter().enumerate().find(|(_, target)| {
            target.canonical_path() == reference.module && target.version() == reference.version
        })?;
        let export = reference.export.as_deref()?;
        let symbol = *target
            .exports()
            .iter()
            .find(|id| target.hir().name(target.symbols()[id.0 as usize].name) == export)?;
        let function = target
            .functions()
            .iter()
            .position(|function| target.facts()[function.node.0 as usize].symbol == Some(symbol))?;
        return Some(EffectFunction {
            module,
            function: h::FunctionId(function as u32),
        });
    }
    let fact = current.facts().get(node.0 as usize)?;
    match current.types().get(fact.ty.0 as usize)? {
        h::Type::Function(function) => Some(EffectFunction {
            module,
            function: *function,
        }),
        _ => None,
    }
}

/// Compile a pure function with a fragment first parameter and typed live parameters.
/// This entry does not register author APIs, execute a VM, or prepare a GPU program.
pub fn compile_effect(
    modules: &[h::AnalyzedModule],
    entry: EffectFunction,
    limits: EffectCompileLimits,
) -> Result<CompiledEffect, EffectDiagnostic> {
    let Some(module) = modules.get(entry.module) else {
        return Err(EffectDiagnostic {
            module: String::new(),
            span: TextRange::empty(0.into()),
            message: "unknown effect module".into(),
        });
    };
    let Some(function) = module.functions().get(entry.function.0 as usize) else {
        return Err(EffectDiagnostic {
            module: module.source_path().into(),
            span: TextRange::empty(0.into()),
            message: "unknown effect function".into(),
        });
    };
    let site = Site(entry.module, function.node);
    let mut compiler = Compiler {
        modules,
        limits,
        work: 0,
        nodes: Vec::new(),
        sources: Vec::new(),
        constants: HashMap::new(),
        active: Vec::new(),
    };
    if function.parameters.is_empty() {
        return Err(compiler.error(site, "effect function requires a fragment first parameter"));
    }
    let mut parameters = Vec::new();
    let mut arguments = vec![Value::Fragment];
    let mut defaults = Frame {
        module: entry.module,
        values: HashMap::new(),
    };
    for parameter in function.parameters.iter().skip(1) {
        let symbol = &module.symbols()[parameter.symbol.0 as usize];
        let ty = drawing_type(module, symbol.ty).ok_or_else(|| EffectDiagnostic {
            module: module.source_path().into(),
            span: symbol.declaration,
            message: "effect parameters require bool, float, or a float tuple of length 2 to 4"
                .into(),
        })?;
        let default = if let Some(node) = parameter.default {
            let value = compiler.expression(&mut defaults, node)?;
            compiler.contract(Site(entry.module, node), symbol.ty, &value)?;
            Some(compiler.constant_value(&value).ok_or_else(|| {
                compiler.error(
                    Site(entry.module, node),
                    "effect parameter default must be a finite constant",
                )
            })?)
        } else {
            None
        };
        parameters.push(EffectParameter {
            name: module.hir().name(symbol.name).into(),
            ty,
            default,
        });
        let node = compiler.push(site, ty, Op::Parameter((parameters.len() - 1) as u32))?;
        compiler.sources[node.0 as usize].span = symbol.declaration;
        arguments.push(Value::Node(node));
    }
    let result = compiler.function(entry, arguments, site)?;
    let output = compiler.node(site, result)?;
    let nodes = std::mem::take(&mut compiler.nodes);
    let program = EffectProgram::new(parameters, nodes, output).map_err(|error| {
        let source = match &error {
            reel_ui::stage::effect::EffectValidationError::Node { id, .. } => {
                compiler.sources.get(id.0 as usize).copied()
            }
            _ => None,
        };
        source.map_or_else(
            || compiler.error(site, error.to_string()),
            |source| EffectDiagnostic {
                module: modules[source.module].source_path().into(),
                span: source.span,
                message: error.to_string(),
            },
        )
    })?;
    Ok(CompiledEffect {
        entry,
        program,
        node_sources: compiler.sources,
        source_files: modules
            .iter()
            .map(|module| Arc::from(module.source_path()))
            .collect(),
    })
}

fn drawing_type(module: &h::AnalyzedModule, ty: h::TypeId) -> Option<Ty> {
    match &module.types()[ty.0 as usize] {
        h::Type::Bool => Some(Ty::Bool),
        h::Type::Float => Some(Ty::Float),
        h::Type::Tuple(items)
            if items
                .iter()
                .all(|ty| matches!(module.types()[ty.0 as usize], h::Type::Float)) =>
        {
            match items.len() {
                2 => Some(Ty::Vec2),
                3 => Some(Ty::Vec3),
                4 => Some(Ty::Vec4),
                _ => None,
            }
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Site(usize, h::NodeId);
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Bool(bool),
    Int(i64),
    Float(f32),
    Node(Id),
    Tuple(Vec<Value>),
    Fragment,
    Function(EffectFunction),
    Native { module: usize, binding: usize },
    Null,
}
#[derive(Clone)]
struct Frame {
    module: usize,
    values: HashMap<h::SymbolId, Value>,
}

struct Compiler<'a> {
    modules: &'a [h::AnalyzedModule],
    limits: EffectCompileLimits,
    work: usize,
    nodes: Vec<EffectNode>,
    sources: Vec<EffectSource>,
    constants: HashMap<Constant, Id>,
    active: Vec<EffectFunction>,
}
impl Compiler<'_> {
    fn error(&self, site: Site, message: impl Into<String>) -> EffectDiagnostic {
        EffectDiagnostic {
            module: self.modules[site.0].source_path().into(),
            span: self.modules[site.0].hir().node(site.1).span,
            message: message.into(),
        }
    }
    fn work(&mut self, site: Site) -> Result<(), EffectDiagnostic> {
        self.work += 1;
        if self.work > self.limits.work {
            Err(self.error(site, "effect source expansion work limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn push(&mut self, site: Site, ty: Ty, op: Op) -> Result<Id, EffectDiagnostic> {
        if self.nodes.len() >= self.limits.nodes.min(u32::MAX as usize) {
            return Err(self.error(site, "effect expression capacity exceeded"));
        }
        let id = Id(self.nodes.len() as u32);
        self.nodes.push(EffectNode { ty, op });
        self.sources.push(EffectSource {
            module: site.0,
            span: self.modules[site.0].hir().node(site.1).span,
        });
        Ok(id)
    }
    fn constant_value(&self, value: &Value) -> Option<Constant> {
        let number = |value: &Value| match value {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f32),
            _ => None,
        };
        let value = match value {
            Value::Bool(v) => Constant::Bool(*v),
            Value::Float(v) => Constant::Float(v.to_bits()),
            Value::Int(v) => Constant::Float((*v as f32).to_bits()),
            Value::Tuple(values) => {
                let values = values.iter().map(number).collect::<Option<Vec<_>>>()?;
                match values.as_slice() {
                    [a, b] => Constant::Vec2([a.to_bits(), b.to_bits()]),
                    [a, b, c] => Constant::Vec3([a.to_bits(), b.to_bits(), c.to_bits()]),
                    [a, b, c, d] => {
                        Constant::Vec4([a.to_bits(), b.to_bits(), c.to_bits(), d.to_bits()])
                    }
                    _ => return None,
                }
            }
            Value::Node(id) => match self.nodes[id.0 as usize].op {
                Op::Constant(value) => value,
                _ => return None,
            },
            _ => return None,
        };
        value.is_finite().then_some(value)
    }
    fn node(&mut self, site: Site, value: Value) -> Result<Id, EffectDiagnostic> {
        if let Value::Node(id) = value {
            return Ok(id);
        }
        if let Some(value) = self.constant_value(&value) {
            if let Some(&id) = self.constants.get(&value) {
                return Ok(id);
            }
            let id = self.push(site, value.ty(), Op::Constant(value))?;
            self.constants.insert(value, id);
            return Ok(id);
        }
        if let Value::Tuple(values) = value {
            let values = values
                .into_iter()
                .map(|v| self.node(site, v))
                .collect::<Result<Vec<_>, _>>()?;
            if values
                .iter()
                .any(|v| self.nodes[v.0 as usize].ty != Ty::Float)
            {
                return Err(self.error(site, "drawing vectors require float components"));
            }
            return match values.as_slice() {
                [a, b] => self.push(site, Ty::Vec2, Op::Vec2([*a, *b])),
                [a, b, c] => self.push(site, Ty::Vec3, Op::Vec3([*a, *b, *c])),
                [a, b, c, d] => self.push(site, Ty::Vec4, Op::Vec4([*a, *b, *c, *d])),
                _ => Err(self.error(site, "drawing vectors have 2 to 4 components")),
            };
        }
        Err(self.error(site, "expression cannot produce a drawing value"))
    }
    fn select(
        &mut self,
        site: Site,
        condition: Value,
        yes: Value,
        no: Value,
    ) -> Result<Value, EffectDiagnostic> {
        if let Value::Bool(value) = condition {
            return Ok(if value { yes } else { no });
        }
        let condition = self.node(site, condition)?;
        if self.nodes[condition.0 as usize].ty != Ty::Bool {
            return Err(self.error(site, "if requires a bool condition"));
        }
        if yes == no {
            return Ok(yes);
        }
        let when_true = self.node(site, yes)?;
        let when_false = self.node(site, no)?;
        let ty = self.nodes[when_true.0 as usize].ty;
        if self.nodes[when_false.0 as usize].ty != ty {
            return Err(self.error(
                site,
                "if requires a bool condition and matching branch types",
            ));
        }
        Ok(Value::Node(self.push(
            site,
            ty,
            Op::Select {
                condition,
                when_true,
                when_false,
            },
        )?))
    }
}

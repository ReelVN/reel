// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Typed, immutable expressions for scalar coverage and linear premultiplied color.
//!
//! Nodes describe expressions rather than an execution schedule. Consumers start at
//! the output and preserve [`EffectOp::Select`] branches; evaluating every node in
//! storage order would execute expressions from an unselected branch. Functions and
//! fixed loops are expanded by the source compiler before constructing this graph.
//! Source locations belong to the host's node-index map, keeping this module free
//! of language and device dependencies.

use std::{
    collections::BTreeSet,
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};
mod mask;
pub use mask::lower_mask;

/// Drawing values use float32 components; vectors correspond to fixed-size tuples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectType {
    Bool,
    Float,
    Vec2,
    Vec3,
    Vec4,
}

impl EffectType {
    fn numeric(self) -> bool {
        self != Self::Bool
    }

    fn lanes(self) -> usize {
        match self {
            Self::Bool | Self::Float => 1,
            Self::Vec2 => 2,
            Self::Vec3 => 3,
            Self::Vec4 => 4,
        }
    }
}

/// Float constants retain their exact finite float32 bit patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectValue {
    Bool(bool),
    Float(u32),
    Vec2([u32; 2]),
    Vec3([u32; 3]),
    Vec4([u32; 4]),
}

impl EffectValue {
    /// Return the value's declared drawing type.
    pub fn ty(self) -> EffectType {
        match self {
            Self::Bool(_) => EffectType::Bool,
            Self::Float(_) => EffectType::Float,
            Self::Vec2(_) => EffectType::Vec2,
            Self::Vec3(_) => EffectType::Vec3,
            Self::Vec4(_) => EffectType::Vec4,
        }
    }

    /// Check the same finite-component boundary used for constants and parameters.
    pub fn is_finite(self) -> bool {
        let finite = |bits| f32::from_bits(bits).is_finite();
        match self {
            Self::Bool(_) => true,
            Self::Float(bits) => finite(bits),
            Self::Vec2(bits) => bits.into_iter().all(finite),
            Self::Vec3(bits) => bits.into_iter().all(finite),
            Self::Vec4(bits) => bits.into_iter().all(finite),
        }
    }
}

/// Content identity of one exported immutable effect definition.
pub type EffectDefinitionId = [u8; 32];

/// Resolved author values for one immutable definition. Asset preparation binds
/// its shader separately, so stage projection does not depend on an asset IO host.
#[derive(Debug)]
pub struct EffectInstance {
    definition: EffectDefinitionId,
    program: Arc<EffectProgram>,
    values: crate::peniko::Blob<EffectValue>,
    values_storage: usize,
    textures: Vec<Arc<str>>,
}
impl PartialEq for EffectInstance {
    fn eq(&self, other: &Self) -> bool {
        self.definition == other.definition
            && self.program == other.program
            && self.values() == other.values()
            && self.textures == other.textures
    }
}
impl Eq for EffectInstance {}

impl EffectInstance {
    /// Admit the complete parameter list in declaration order. Defaults are
    /// resolved by the caller; every supplied value must be finite and typed.
    pub fn new(
        definition: EffectDefinitionId,
        program: Arc<EffectProgram>,
        values: Vec<EffectValue>,
    ) -> Result<Arc<Self>, EffectInstanceError> {
        Self::with_textures(definition, program, values, Vec::new())
    }

    /// Create an immutable numeric/image parameter tuple for one static definition.
    pub fn with_textures(
        definition: EffectDefinitionId,
        program: Arc<EffectProgram>,
        values: Vec<EffectValue>,
        textures: Vec<Arc<str>>,
    ) -> Result<Arc<Self>, EffectInstanceError> {
        let storage = size_of::<Vec<EffectValue>>()
            + 2 * size_of::<usize>()
            + values.capacity() * size_of::<EffectValue>();
        Self::from_owned_inputs(
            definition,
            program,
            crate::peniko::Blob::new(Arc::new(values)),
            storage,
            textures,
        )
    }

    /// Retain typed parameters with their host-owned storage and final-release
    /// token. `storage_bytes` covers the blob owner, its Arc header and payload,
    /// excluding the separately shared program and this instance's allocation.
    pub fn from_owned_values(
        definition: EffectDefinitionId,
        program: Arc<EffectProgram>,
        values: crate::peniko::Blob<EffectValue>,
        storage_bytes: usize,
    ) -> Result<Arc<Self>, EffectInstanceError> {
        Self::from_owned_inputs(definition, program, values, storage_bytes, Vec::new())
    }

    /// Admit numeric values and whole-image asset sources as separate typed inputs.
    /// Sources retain their logical asset names; preparation owns decoded leases.
    pub fn from_owned_inputs(
        definition: EffectDefinitionId,
        program: Arc<EffectProgram>,
        values: crate::peniko::Blob<EffectValue>,
        storage_bytes: usize,
        textures: Vec<Arc<str>>,
    ) -> Result<Arc<Self>, EffectInstanceError> {
        if textures.len() != program.textures().len()
            || textures
                .iter()
                .any(|path| path.is_empty() || path.ends_with(".reel-image"))
        {
            return Err(EffectInstanceError::Parameter {
                index: values.len(),
                reason: "texture inputs must name whole images or prepared textures",
            });
        }
        if values.len() != program.parameters().len() {
            return Err(EffectInstanceError::Count {
                expected: program.parameters().len(),
                actual: values.len(),
            });
        }
        for (index, (value, parameter)) in
            values.data().iter().zip(program.parameters()).enumerate()
        {
            if value.ty() != parameter.ty {
                return Err(EffectInstanceError::Parameter {
                    index,
                    reason: "value type differs from the declaration",
                });
            }
            if !value.is_finite() {
                return Err(EffectInstanceError::Parameter {
                    index,
                    reason: "value components must be finite",
                });
            }
        }
        Ok(Arc::new(Self {
            definition,
            program,
            values,
            values_storage: storage_bytes,
            textures,
        }))
    }

    /// Exported identity used to resolve the platform shader asset.
    pub fn definition(&self) -> EffectDefinitionId {
        self.definition
    }

    /// Shared expression topology and parameter declarations.
    pub fn program(&self) -> &Arc<EffectProgram> {
        &self.program
    }

    /// Current immutable parameter values in declaration order.
    pub fn values(&self) -> &[EffectValue] {
        self.values.data()
    }

    /// Whole-image asset names in texture declaration order, independent of uniforms.
    pub fn textures(&self) -> &[Arc<str>] {
        &self.textures
    }

    /// Instance storage excluding the separately shared program allocation.
    pub fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.values_storage
            + self.textures.capacity() * size_of::<Arc<str>>()
            + self
                .textures
                .iter()
                .map(|path| path.len() + 2 * size_of::<usize>())
                .sum::<usize>()
    }
}

/// Invalid prepared parameters, before stage publication or shader submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectInstanceError {
    Count { expected: usize, actual: usize },
    Parameter { index: usize, reason: &'static str },
}
impl fmt::Display for EffectInstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count { expected, actual } => {
                write!(f, "effect expects {expected} parameters, got {actual}")
            }
            Self::Parameter { index, reason } => write!(f, "effect parameter {index}: {reason}"),
        }
    }
}
impl std::error::Error for EffectInstanceError {}

/// A typed live parameter. Defaults initialize an instance rather than shader code.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectParameter {
    pub name: Arc<str>,
    pub ty: EffectType,
    pub default: Option<EffectValue>,
}

/// A whole-image resource parameter. Texture bindings never occupy numeric slots.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectTextureParameter {
    /// Author-facing parameter name shared by source checking and instance binding.
    pub name: Arc<str>,
}

/// The result requested from a whole-image resource at normalized coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectTextureChannel {
    /// Linear premultiplied RGBA.
    Color,
    /// Image coverage, including compressed-format alpha rules.
    Alpha,
    /// The existing mask grayscale channel after filtering and color decoding.
    Gray,
}

/// An expression identity within one program; references always point backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EffectNodeId(pub u32);

/// Per-node drawing context. Sharing a program never shares another node's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectInput {
    /// Normalized coordinates within the current logical image or rectangle.
    Uv,
    /// Coordinates in the current node's logical pixels.
    Position,
    /// Current logical width and height.
    Size,
}

/// Pure component-wise math, except Length and Normalize, which use whole vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectUnary {
    Negate,
    Abs,
    Floor,
    Ceil,
    Fract,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sqrt,
    Exp,
    Log,
    Exp2,
    Log2,
    Length,
    Normalize,
    Not,
}

/// Arithmetic uses equal numeric types. Tuple/scalar expansion is explicit in IR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectBinary {
    Add,
    Subtract,
    Multiply,
    Divide,
    /// Floating remainder with a quotient rounded toward zero, matching Aside.
    Remainder,
    Pow,
    Min,
    Max,
    /// atan2(y, x), preserving the quadrant.
    Atan2,
    Dot,
    Distance,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

/// Component-wise three-argument math with equal numeric operand types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectTernary {
    /// clamp(value, minimum, maximum).
    Clamp,
    /// mix(left, right, weight).
    Mix,
    /// smoothstep(edge0, edge1, value).
    Smoothstep,
}

/// A pure expression. Backend code generation specializes its actual operands.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectOp {
    Constant(EffectValue),
    Parameter(u32),
    Input(EffectInput),
    /// Sample this node's source through its atlas window and color decoding.
    /// The result is linear premultiplied RGBA; coordinates have type Vec2.
    SourceSample {
        uv: EffectNodeId,
    },
    /// Sample one declared texture binding. Coordinates have type Vec2 and clamp
    /// to the content's texel centers; channel controls the typed result.
    TextureSample {
        binding: u32,
        uv: EffectNodeId,
        channel: EffectTextureChannel,
    },
    Vec2([EffectNodeId; 2]),
    Vec3([EffectNodeId; 3]),
    Vec4([EffectNodeId; 4]),
    Extract {
        value: EffectNodeId,
        lane: u8,
    },
    Unary {
        op: EffectUnary,
        value: EffectNodeId,
    },
    Binary {
        op: EffectBinary,
        left: EffectNodeId,
        right: EffectNodeId,
    },
    Ternary {
        op: EffectTernary,
        a: EffectNodeId,
        b: EffectNodeId,
        c: EffectNodeId,
    },
    /// Evaluate condition, then only the selected expression and its dependencies.
    /// Both branches have the same type; the condition has type Bool.
    Select {
        condition: EffectNodeId,
        when_true: EffectNodeId,
        when_false: EffectNodeId,
    },
}

/// A typed expression whose source location is indexed by the host.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EffectNode {
    pub ty: EffectType,
    pub op: EffectOp,
}

/// Admission failure at an expression, parameter, or the program output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectValidationError {
    Storage,
    Parameter {
        index: usize,
        reason: &'static str,
    },
    Node {
        id: EffectNodeId,
        reason: &'static str,
    },
    Output {
        id: EffectNodeId,
    },
}

impl fmt::Display for EffectValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage => f.write_str("invalid effect expression storage"),
            Self::Parameter { index, reason } => write!(f, "effect parameter {index}: {reason}"),
            Self::Node { id, reason } => write!(f, "effect node {}: {reason}", id.0),
            Self::Output { id } => write!(f, "effect output {} must be scalar or RGBA", id.0),
        }
    }
}

impl std::error::Error for EffectValidationError {}

/// One validated expression graph, shared by coverage and color consumers.
#[derive(Debug, PartialEq, Eq)]
pub struct EffectProgram {
    parameters: Vec<EffectParameter>,
    textures: Vec<EffectTextureParameter>,
    nodes: Vec<EffectNode>,
    output: EffectNodeId,
    structure_hash: u64,
}

impl EffectProgram {
    /// Validate structure once before sharing it. Source diagnostics map node ids
    /// back to their expressions. Target resource limits are applied during prepare.
    pub fn new(
        parameters: Vec<EffectParameter>,
        nodes: Vec<EffectNode>,
        output: EffectNodeId,
    ) -> Result<Arc<Self>, EffectValidationError> {
        Self::with_textures(parameters, Vec::new(), nodes, output)
    }

    /// Validate numeric expressions and separately declared texture bindings.
    pub fn with_textures(
        parameters: Vec<EffectParameter>,
        textures: Vec<EffectTextureParameter>,
        nodes: Vec<EffectNode>,
        output: EffectNodeId,
    ) -> Result<Arc<Self>, EffectValidationError> {
        if nodes.is_empty()
            || nodes.len() > u32::MAX as usize
            || parameters.len() > u32::MAX as usize
            || textures.len() > u32::MAX as usize
        {
            return Err(EffectValidationError::Storage);
        }
        let mut names = BTreeSet::new();
        for (index, parameter) in parameters.iter().enumerate() {
            let invalid = |reason| EffectValidationError::Parameter { index, reason };
            if parameter.name.is_empty() || !names.insert(parameter.name.as_ref()) {
                return Err(invalid("names must be nonempty and unique"));
            }
            if let Some(value) = parameter.default
                && (value.ty() != parameter.ty || !value.is_finite())
            {
                return Err(invalid(
                    "default must have the declared type and finite components",
                ));
            }
        }
        for (index, texture) in textures.iter().enumerate() {
            if texture.name.is_empty() || !names.insert(texture.name.as_ref()) {
                return Err(EffectValidationError::Parameter {
                    index,
                    reason: "names must be nonempty and unique",
                });
            }
        }
        for (index, node) in nodes.iter().enumerate() {
            let id = EffectNodeId(index as u32);
            let ty = node
                .op
                .result_type(&nodes[..index], &parameters)
                .map_err(|reason| EffectValidationError::Node { id, reason })?;
            if let EffectOp::TextureSample { binding, .. } = node.op
                && binding as usize >= textures.len()
            {
                return Err(EffectValidationError::Node {
                    id,
                    reason: "unknown texture binding",
                });
            }
            if ty != node.ty {
                return Err(EffectValidationError::Node {
                    id,
                    reason: "declared result type differs from the expression",
                });
            }
        }
        if !matches!(
            nodes.get(output.0 as usize).map(|node| node.ty),
            Some(EffectType::Float | EffectType::Vec4)
        ) {
            return Err(EffectValidationError::Output { id: output });
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        parameters.len().hash(&mut hash);
        textures.len().hash(&mut hash);
        for parameter in &parameters {
            parameter.ty.hash(&mut hash);
        }
        nodes.hash(&mut hash);
        output.hash(&mut hash);
        Ok(Arc::new(Self {
            parameters,
            textures,
            nodes,
            output,
            structure_hash: hash.finish(),
        }))
    }

    /// Parameter declarations in stable slot order.
    pub fn parameters(&self) -> &[EffectParameter] {
        &self.parameters
    }

    /// Texture resource declarations in stable binding order.
    pub fn textures(&self) -> &[EffectTextureParameter] {
        &self.textures
    }

    /// Expressions in dependency order. This order is not an execution schedule.
    pub fn nodes(&self) -> &[EffectNode] {
        &self.nodes
    }

    /// Start evaluation and code generation from this expression.
    pub fn output(&self) -> EffectNodeId {
        self.output
    }

    /// In-process cache key for structure and parameter types. Compare
    /// `same_structure` after a hit; package content hashes use their wire encoding.
    pub fn structure_hash(&self) -> u64 {
        self.structure_hash
    }

    /// Program reuse ignores parameter names and defaults, which belong to instances.
    pub fn same_structure(&self, other: &Self) -> bool {
        self.output == other.output
            && self.textures.len() == other.textures.len()
            && self.nodes == other.nodes
            && self
                .parameters
                .iter()
                .map(|p| p.ty)
                .eq(other.parameters.iter().map(|p| p.ty))
    }

    /// Owned structural capacity, for the caller's shared program budget ticket.
    /// Each shared program is charged once, independently of its consumer count.
    pub fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.parameters.capacity() * size_of::<EffectParameter>()
            + self.textures.capacity() * size_of::<EffectTextureParameter>()
            + self
                .textures
                .iter()
                .map(|p| p.name.len() + 2 * size_of::<usize>())
                .sum::<usize>()
            + self.nodes.capacity() * size_of::<EffectNode>()
            + self
                .parameters
                .iter()
                .map(|p| p.name.len() + 2 * size_of::<usize>())
                .sum::<usize>()
    }
}

impl EffectOp {
    fn result_type(
        &self,
        nodes: &[EffectNode],
        parameters: &[EffectParameter],
    ) -> Result<EffectType, &'static str> {
        let get = |id: EffectNodeId| {
            nodes
                .get(id.0 as usize)
                .map(|node| node.ty)
                .ok_or("reference must point to a preceding expression")
        };
        let numeric = |ty: EffectType| {
            if ty.numeric() {
                Ok(ty)
            } else {
                Err("expected numeric operands")
            }
        };
        let vector = |ty: EffectType| {
            if ty.lanes() > 1 {
                Ok(ty)
            } else {
                Err("expected a vector operand")
            }
        };
        let construct = |ids: &[EffectNodeId], ty| {
            for &id in ids {
                if get(id)? != EffectType::Float {
                    return Err("vector components must be float expressions");
                }
            }
            Ok(ty)
        };
        match *self {
            Self::Constant(value) => {
                if value.is_finite() {
                    Ok(value.ty())
                } else {
                    Err("constant must be finite")
                }
            }
            Self::Parameter(index) => parameters
                .get(index as usize)
                .map(|parameter| parameter.ty)
                .ok_or("unknown parameter slot"),
            Self::Input(_) => Ok(EffectType::Vec2),
            Self::SourceSample { uv } => {
                if get(uv)? == EffectType::Vec2 {
                    Ok(EffectType::Vec4)
                } else {
                    Err("source sample coordinates must be Vec2")
                }
            }
            Self::TextureSample { uv, channel, .. } => {
                if get(uv)? != EffectType::Vec2 {
                    return Err("texture sample coordinates must be Vec2");
                }
                Ok(if channel == EffectTextureChannel::Color {
                    EffectType::Vec4
                } else {
                    EffectType::Float
                })
            }
            Self::Vec2(ids) => construct(&ids, EffectType::Vec2),
            Self::Vec3(ids) => construct(&ids, EffectType::Vec3),
            Self::Vec4(ids) => construct(&ids, EffectType::Vec4),
            Self::Extract { value, lane } => {
                let ty = vector(get(value)?)?;
                if usize::from(lane) < ty.lanes() {
                    Ok(EffectType::Float)
                } else {
                    Err("vector component is out of range")
                }
            }
            Self::Unary { op, value } => {
                let ty = get(value)?;
                match op {
                    EffectUnary::Not if ty == EffectType::Bool => Ok(ty),
                    EffectUnary::Not => Err("logical negation requires Bool"),
                    EffectUnary::Length => vector(ty).map(|_| EffectType::Float),
                    EffectUnary::Normalize => vector(ty),
                    _ => numeric(ty),
                }
            }
            Self::Binary { op, left, right } => {
                let ty = get(left)?;
                if ty != get(right)? {
                    return Err("binary operand types differ");
                }
                match op {
                    EffectBinary::Equal | EffectBinary::NotEqual => Ok(EffectType::Bool),
                    EffectBinary::Less
                    | EffectBinary::LessEqual
                    | EffectBinary::Greater
                    | EffectBinary::GreaterEqual => {
                        if ty == EffectType::Float {
                            Ok(EffectType::Bool)
                        } else {
                            Err("ordered comparison requires float operands")
                        }
                    }
                    EffectBinary::Dot | EffectBinary::Distance => {
                        vector(ty).map(|_| EffectType::Float)
                    }
                    _ => numeric(ty),
                }
            }
            Self::Ternary { a, b, c, .. } => {
                let ty = numeric(get(a)?)?;
                if ty == get(b)? && ty == get(c)? {
                    Ok(ty)
                } else {
                    Err("ternary operand types differ")
                }
            }
            Self::Select {
                condition,
                when_true,
                when_false,
            } => {
                if get(condition)? != EffectType::Bool {
                    return Err("branch condition must be Bool");
                }
                let ty = get(when_true)?;
                if ty == get(when_false)? {
                    Ok(ty)
                } else {
                    Err("branch result types differ")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(value: EffectValue) -> EffectNode {
        EffectNode {
            ty: value.ty(),
            op: EffectOp::Constant(value),
        }
    }

    #[test]
    fn coverage_and_color_share_typed_program_admission() {
        let coverage = EffectProgram::new(
            vec![],
            vec![constant(EffectValue::Float(0.5_f32.to_bits()))],
            EffectNodeId(0),
        )
        .unwrap();
        assert_eq!(coverage.nodes()[0].ty, EffectType::Float);
        let color = EffectProgram::new(
            vec![],
            vec![
                EffectNode {
                    ty: EffectType::Vec2,
                    op: EffectOp::Input(EffectInput::Uv),
                },
                EffectNode {
                    ty: EffectType::Vec4,
                    op: EffectOp::SourceSample {
                        uv: EffectNodeId(0),
                    },
                },
            ],
            EffectNodeId(1),
        )
        .unwrap();
        assert_eq!(
            color.nodes()[color.output().0 as usize].ty,
            EffectType::Vec4
        );
        assert!(
            EffectProgram::new(
                vec![],
                vec![EffectNode {
                    ty: EffectType::Float,
                    op: EffectOp::SourceSample {
                        uv: EffectNodeId(0)
                    }
                },],
                EffectNodeId(0)
            )
            .is_err()
        );
        for bits in [
            f32::INFINITY.to_bits(),
            f32::NEG_INFINITY.to_bits(),
            f32::NAN.to_bits(),
        ] {
            assert!(
                EffectProgram::new(
                    vec![],
                    vec![constant(EffectValue::Vec4([0, 0, bits, 0]))],
                    EffectNodeId(0)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn branches_keep_typed_backwards_references() {
        let nodes = vec![
            constant(EffectValue::Bool(true)),
            constant(EffectValue::Float(1_f32.to_bits())),
            constant(EffectValue::Float((-1_f32).to_bits())),
            EffectNode {
                ty: EffectType::Float,
                op: EffectOp::Unary {
                    op: EffectUnary::Sqrt,
                    value: EffectNodeId(2),
                },
            },
            EffectNode {
                ty: EffectType::Float,
                op: EffectOp::Select {
                    condition: EffectNodeId(0),
                    when_true: EffectNodeId(1),
                    when_false: EffectNodeId(3),
                },
            },
        ];
        // sqrt(-1) belongs to the unselected expression. Admission preserves the
        // graph without evaluating it or treating storage order as eager execution.
        let program = EffectProgram::new(vec![], nodes.clone(), EffectNodeId(4)).unwrap();
        assert_eq!(program.nodes(), nodes);
        for op in [
            EffectOp::Select {
                condition: EffectNodeId(1),
                when_true: EffectNodeId(1),
                when_false: EffectNodeId(3),
            },
            EffectOp::Select {
                condition: EffectNodeId(0),
                when_true: EffectNodeId(1),
                when_false: EffectNodeId(0),
            },
            EffectOp::Select {
                condition: EffectNodeId(0),
                when_true: EffectNodeId(1),
                when_false: EffectNodeId(5),
            },
        ] {
            let mut invalid = nodes.clone();
            invalid[4].op = op;
            assert!(matches!(
                EffectProgram::new(vec![], invalid, EffectNodeId(4)),
                Err(EffectValidationError::Node {
                    id: EffectNodeId(4),
                    ..
                })
            ));
        }
    }

    #[test]
    fn program_reuse_tracks_parameter_types_without_baking_instance_defaults() {
        let program = |name: &str, value: EffectValue| {
            EffectProgram::new(
                vec![EffectParameter {
                    name: name.into(),
                    ty: value.ty(),
                    default: Some(value),
                }],
                vec![EffectNode {
                    ty: value.ty(),
                    op: EffectOp::Parameter(0),
                }],
                EffectNodeId(0),
            )
            .unwrap()
        };
        let first = program("time", EffectValue::Float(0));
        let changed = program("phase", EffectValue::Float(8_f32.to_bits()));
        assert!(first.same_structure(&changed));
        assert_eq!(first.structure_hash(), changed.structure_hash());
        let color = program("tint", EffectValue::Vec4([0; 4]));
        assert!(!first.same_structure(&color));
        assert_ne!(first.structure_hash(), color.structure_hash());
    }
}

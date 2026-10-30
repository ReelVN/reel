//! Static shader generation for the shared typed drawing-expression graph.
//!
//! [`compile_fragment`] supplies the production source hook and entry point;
//! [`generate`] exposes the same expression body for CPU tooling. Source reads
//! return linear premultiplied color at base level, including nonuniform branches.
//!
//! All target languages use float32 operations. Remainder lowers to
//! `a - b * (sign(a / b) * floor(abs(a / b)))`, with the quotient evaluated once;
//! this is a truncating-quotient expression, not a bit-exact IEEE fmod routine.
//! Smoothstep uses a step at equal edges and the same clamped cubic for ordered
//! and reversed edges. Hardware transcendental precision remains target-specific.

use reel_ui::stage::effect::{
    EffectBinary, EffectInput, EffectNodeId, EffectOp, EffectProgram, EffectTernary,
    EffectTextureChannel, EffectType, EffectUnary, EffectValue,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[path = "effect/cg.rs"]
mod cg;
#[path = "effect/emit_reduction.rs"]
mod emit_reduction;
#[path = "effect/footprint.rs"]
mod footprint;
pub use footprint::source_footprint;
#[path = "effect/gxp_compiler.rs"]
pub mod gxp_compiler;
#[path = "effect/reduction.rs"]
mod reduction;
#[path = "effect/wgsl.rs"]
mod wgsl;

/// The production source binding is ten float32 vec4 slots (160 bytes).
/// Slots 0/1 map target pixels to local normalized coordinates; 2/3 map local
/// normalized coordinates to source texture coordinates. Slot 4 is the logical
/// rectangle (x, y, width, height); 5 is target size and source storage size;
/// 6 is the source atlas window (origin x/y, extent x/y), in normalized units.
/// Slots 7/8 map normalized target coordinates directly to the whole local
/// source for the wrapper's implicit current-pixel sample.
/// Slot 0.w selects ordinary output (zero), scalar coverage (one), or inverted
/// scalar coverage (two), or source multiplied by inverted coverage (three).
/// Inversion follows coverage clamping.
/// Slot 1.w selects a terminal source-over output: zero keeps ordinary output,
/// one quantizes to RGBA8 UNORM, two rounds to binary16. Slot 2.w carries its
/// opacity. Quantization precedes opacity, preserving the intermediate color
/// domain. RGBA8 halfway ties permit one UNORM unit per intermediate channel;
/// source-over bounds the resulting RGB error by two units and alpha by one.
/// Terminal output is selected only for single-sample, uninverted source-over.
/// Slot 3.w requests a second attachment quantization after opacity, before
/// coverage, preserving an enclosing isolated clip target. Two UNORM tie stages
/// bound final source-over RGB/alpha differences by four/two UNORM units.
/// Slot 9 maps normalized output coordinates to the independent coverage texture.
pub const CONTEXT_SLOTS: usize = 10;

/// Fixed post-effect operation, shared by planning and both shader generators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTail {
    /// Ordinary output, or attachment quantization followed by opacity/source-over.
    Ordinary,
    /// Attachment quantization, independent clamped alpha coverage, then opacity.
    AlphaCoverage,
}
impl OutputTail {
    /// Portable GXM/WGPU sampler admission; color output alone accepts coverage.
    pub fn supports_coverage(output: EffectType, textures: u32, source: bool) -> bool {
        output == EffectType::Vec4 && textures.saturating_add(u32::from(source)) < 16
    }
}

/// Raster coverage guaranteed by the caller's shared render plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentCoverage {
    /// Keep the logical UV domain check and native pixel-scissor discard.
    Local,
    /// The ordinary output covers the physical attachment. The caller proves
    /// the complete viewport, scissor and output bounds. The original UV check
    /// returns transparent color outside its domain, preserving clear followed
    /// by local discard; native pixel-scissor discard is omitted.
    FullTarget,
}

/// Complete prepared shader source; runtime backends load this product verbatim.
#[derive(Clone, Debug)]
pub struct EffectFragment {
    pub source: String,
    pub parameters: ParameterLayout,
    pub output_type: EffectType,
    /// Whether the complete entry point references the subtree source sampler.
    pub source_sampler: bool,
}

/// Export the complete v1 fragment ABI without requiring a GPU feature.
///
/// WGSL includes the fullscreen `vertex` and `fragment` entries and group zero bindings: source
/// texture 0 when used, linear clamp sampler 1, context 2, numeric parameters 3,
/// texture metadata 4, and fixed external texture bindings starting at 5. Texture
/// parameters have no numeric uniform slots. Each image has content scale,
/// encoding/format flags and half-texel bounds; generated channel helpers access
/// only the metadata required by their declared sample operations.
/// Effect subtree and prepared-image inputs contain only base level zero. Both
/// targets select that level explicitly, including samples in branches or loops.
/// Sony Cg exposes `reel_context`, `reel_parameter_N` and `reel_texture_meta`
/// as float4 uniforms and uses the `main` fragment entry. GXP retains symbols
/// for parameter reflection. `v_uv` and the implicit source
/// UV use TEXCOORD0/1. `reel_source` occupies TEXUNIT0 when required; external
/// image units follow it in declaration order. `reel_clip_rect` carries the
/// top-left half-open scissor in pixels and is tested against WPOS. All native
/// source texture coordinates use the top-left framebuffer convention.
///
/// Output is restricted to the local logical rectangle. Source samples clamp
/// normalized local coordinates and the source atlas window at texel centers.
/// Source and output are linear premultiplied colors; scalar output multiplies
/// the unmodified whole local source at the current coordinates by coverage.
/// That implicit sample uses an affine `reel_implicit_source_uv` vertex varying
/// and hardware edge clamping. Padded scratch supplies piecewise clamped vertex UVs.
/// Explicit authored samples retain their fragment
/// coordinate evaluation and texel/window clamps.
pub fn compile_fragment(
    program: &EffectProgram,
    language: Language,
    max_source_bytes: usize,
) -> Result<EffectFragment, GenerateError> {
    compile_fragment_with_tail(program, language, OutputTail::Ordinary, max_source_bytes)
}
/// Generate one fixed variant without changing the authored effect graph.
/// If bounded loop blocks exceed the complete source limit, retry the original
/// four-tap and single-tap loops so an optimization cannot reject a previously
/// admitted shader.
pub fn compile_fragment_with_tail(
    program: &EffectProgram,
    language: Language,
    tail: OutputTail,
    max_source_bytes: usize,
) -> Result<EffectFragment, GenerateError> {
    compile_fragment_variant(
        program,
        language,
        tail,
        FragmentCoverage::Local,
        max_source_bytes,
    )
}

/// Generate one coverage-qualified variant of the unchanged author program.
/// `FullTarget` requires a full-attachment proof and accepts only the ordinary
/// tail. UV-domain rejection returns transparent color. Source/image sampling,
/// parameters and attachment quantization retain the local variant's behavior.
/// Both variants retain four-tap and single-tap source-limit fallback.
pub fn compile_fragment_variant(
    program: &EffectProgram,
    language: Language,
    tail: OutputTail,
    coverage: FragmentCoverage,
    max_source_bytes: usize,
) -> Result<EffectFragment, GenerateError> {
    if coverage == FragmentCoverage::FullTarget && tail != OutputTail::Ordinary {
        return Err(GenerateError::UnsupportedTail);
    }
    let compile = |max_block_width| -> Result<_, GenerateError> {
        let generated = generate_inner(program, language, max_source_bytes, max_block_width)?;
        if tail == OutputTail::AlphaCoverage
            && !OutputTail::supports_coverage(
                generated.output_type,
                program.textures().len() as u32,
                generated.samples_source,
            )
        {
            return Err(GenerateError::UnsupportedTail);
        }
        match language {
            Language::Wgsl => wgsl::compile(program, generated, tail, coverage, max_source_bytes),
            Language::Cg => cg::compile(program, generated, tail, coverage, max_source_bytes),
        }
    };
    source_fallback(compile)
}

/// The shader language of a prepared pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// WGSL source for the desktop WGPU renderer.
    Wgsl,
    /// Sony Cg 2.2 source for offline `sce_fp_psp2` GXP compilation.
    Cg,
}

/// One stable uniform slot. Values occupy the first lanes of a sixteen-byte slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterSlot {
    /// Type checked before host values are packed into this slot.
    pub ty: EffectType,
    /// Byte offset from the start of the parameter uniform buffer.
    pub byte_offset: usize,
}

/// Shared host layout; WGSL members and Cg float4 uniforms use identical lanes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterLayout {
    pub slots: Vec<ParameterSlot>,
}

impl ParameterLayout {
    /// Total bytes occupied by the declared sixteen-byte slots.
    pub fn byte_len(&self) -> usize {
        self.slots.len() * 16
    }

    /// Pack already-resolved parameter values; booleans occupy x as zero or one.
    pub fn pack(&self, values: &[EffectValue]) -> Result<Vec<[f32; 4]>, ParameterError> {
        if values.len() != self.slots.len() {
            return Err(ParameterError::Count);
        }
        values
            .iter()
            .zip(&self.slots)
            .enumerate()
            .map(|(index, (&value, slot))| {
                if value.ty() != slot.ty {
                    return Err(ParameterError::Type { index });
                }
                if !value.is_finite() {
                    return Err(ParameterError::NonFinite { index });
                }
                Ok(match value {
                    EffectValue::Bool(value) => [u8::from(value) as f32, 0., 0., 0.],
                    EffectValue::Float(a) => [f32::from_bits(a), 0., 0., 0.],
                    EffectValue::Vec2([a, b]) => [f32::from_bits(a), f32::from_bits(b), 0., 0.],
                    EffectValue::Vec3([a, b, c]) => {
                        [f32::from_bits(a), f32::from_bits(b), f32::from_bits(c), 0.]
                    }
                    EffectValue::Vec4(bits) => bits.map(f32::from_bits),
                })
            })
            .collect()
    }
}

/// A resolved value does not match the immutable uniform layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParameterError {
    Count,
    Type { index: usize },
    NonFinite { index: usize },
}
impl std::fmt::Display for ParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Count => f.write_str("effect parameter count differs from the uniform layout"),
            Self::Type { index } => write!(
                f,
                "effect parameter {index} has a different type from its uniform slot"
            ),
            Self::NonFinite { index } => {
                write!(f, "effect parameter {index} has a nonfinite component")
            }
        }
    }
}
impl std::error::Error for ParameterError {}

/// Shader snippets ready for the backend's source hook and fragment wrapper.
#[derive(Clone, Debug)]
pub struct GeneratedEffect {
    pub parameters: ParameterLayout,
    /// Cg uniform declarations or a WGSL `ReelEffectParameters` struct.
    /// WGSL consumers declare their own binding named `reel_parameters`.
    pub declarations: String,
    pub function: String,
    pub output_type: EffectType,
    pub samples_source: bool,
}

/// Source expansion is bounded before a driver/compiler sees the generated code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerateError {
    UnsupportedTail,
    SourceLimit { limit: usize },
}
impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedTail => {
                f.write_str("coverage tail requires color output and one free texture unit")
            }
            Self::SourceLimit { limit } => {
                write!(f, "effect shader exceeds its {limit}-byte source limit")
            }
        }
    }
}
impl std::error::Error for GenerateError {}

/// Generate static expressions and structured branches, never a pixel interpreter.
///
/// Nodes shared within a dominating scope are evaluated once. Branch-local nodes
/// stay inside their selected branch, including source reads and undefined math.
/// Each scope emits its reachable nodes in backward-reference order, so parallel
/// channel accumulations consume a sample together instead of retaining all
/// sampled vectors until a depth-first channel traversal finishes.
/// Traversal uses an explicit CPU work stack so graph depth cannot overflow it.
/// The byte limit covers the returned declarations and function, including helpers;
/// backend-owned entry points and source hooks have their own admission budget.
/// Simple long sampling reductions use four or five ordered taps per iteration.
/// A source-limit retry caps blocks at four taps, then retains the single-tap loop.
pub fn generate(
    program: &EffectProgram,
    language: Language,
    max_source_bytes: usize,
) -> Result<GeneratedEffect, GenerateError> {
    source_fallback(|width| generate_inner(program, language, max_source_bytes, width))
}

fn source_fallback<T>(
    mut build: impl FnMut(usize) -> Result<T, GenerateError>,
) -> Result<T, GenerateError> {
    match build(5) {
        Err(GenerateError::SourceLimit { .. }) => match build(4) {
            Err(GenerateError::SourceLimit { .. }) => build(1),
            result => result,
        },
        result => result,
    }
}
fn generate_inner(
    program: &EffectProgram,
    language: Language,
    max_source_bytes: usize,
    max_block_width: usize,
) -> Result<GeneratedEffect, GenerateError> {
    // Every declared parameter needs at least one sixteen-byte slot and more
    // than sixteen source bytes. Reject oversized layouts before allocating them.
    if program.parameters().len() > max_source_bytes / 16 {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    let parameters = ParameterLayout {
        slots: program
            .parameters()
            .iter()
            .enumerate()
            .map(|(index, p)| ParameterSlot {
                ty: p.ty,
                byte_offset: index * 16,
            })
            .collect(),
    };
    let mut declarations = String::new();
    if !parameters.slots.is_empty() {
        if language == Language::Wgsl {
            declarations.push_str("struct ReelEffectParameters {\n");
        }
        for index in 0..parameters.slots.len() {
            declarations.push_str(&match language {
                Language::Cg => format!("uniform float4 reel_parameter_{index};\n"),
                Language::Wgsl => format!("    p{index}: vec4<f32>,\n"),
            });
            if declarations.len() > max_source_bytes {
                return Err(GenerateError::SourceLimit {
                    limit: max_source_bytes,
                });
            }
        }
        if language == Language::Wgsl {
            declarations.push_str("}\n");
        }
    }
    if declarations.len() > max_source_bytes {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    let mut emitter = Emitter {
        program,
        language,
        max_bytes: max_source_bytes - declarations.len(),
        limit: max_source_bytes,
        body: String::new(),
        helpers: BTreeMap::new(),
        helper_bytes: 0,
        scopes: vec![Scope::default()],
        next_name: 0,
        samples_source: false,
        reductions: HashMap::new(),
        max_block_width,
    };
    emitter.run()?;
    let output_type = program.nodes()[program.output().0 as usize].ty;
    let return_type = language.ty(output_type);
    let header = match language {
        Language::Cg => {
            format!("{return_type} reel_effect(float2 uv, float2 position, float2 size) {{\n")
        }
        Language::Wgsl => format!(
            "fn reel_effect(uv: vec2<f32>, position: vec2<f32>, size: vec2<f32>) -> {return_type} {{\n"
        ),
    };
    let result = emitter.lookup(0, program.output()).unwrap();
    emitter.line(0, &format!("return reel_n{result};"))?;
    let mut function = emitter.helpers.into_values().collect::<String>();
    function.push_str(&header);
    function.push_str(&emitter.body);
    function.push_str("}\n");
    if declarations.len() + function.len() > max_source_bytes {
        return Err(GenerateError::SourceLimit {
            limit: max_source_bytes,
        });
    }
    Ok(GeneratedEffect {
        parameters,
        declarations,
        function,
        output_type,
        samples_source: emitter.samples_source,
    })
}

impl Language {
    fn ty(self, ty: EffectType) -> &'static str {
        match (self, ty) {
            (_, EffectType::Bool) => "bool",
            (Self::Cg, EffectType::Float) => "float",
            (Self::Cg, EffectType::Vec2) => "float2",
            (Self::Cg, EffectType::Vec3) => "float3",
            (Self::Cg, EffectType::Vec4) => "float4",
            (Self::Wgsl, EffectType::Float) => "f32",
            (Self::Wgsl, EffectType::Vec2) => "vec2<f32>",
            (Self::Wgsl, EffectType::Vec3) => "vec3<f32>",
            (Self::Wgsl, EffectType::Vec4) => "vec4<f32>",
        }
    }

    fn declaration(self, ty: EffectType, name: usize, expression: &str) -> String {
        let ty = self.ty(ty);
        match self {
            Self::Cg => format!("{ty} reel_n{name} = {expression};"),
            Self::Wgsl => format!("let reel_n{name}: {ty} = {expression};"),
        }
    }

    fn mutable(self, ty: EffectType, name: usize) -> String {
        let ty = self.ty(ty);
        match self {
            Self::Cg => format!("{ty} reel_n{name};"),
            Self::Wgsl => format!("var reel_n{name}: {ty};"),
        }
    }
}

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    depth: usize,
    values: HashMap<EffectNodeId, usize>,
    /// A reduction iteration substitutes its induction literal. Values that
    /// depend on it must be recomputed even if the original first tap was also
    /// demanded by an outer expression.
    blocked: BTreeSet<u32>,
}

enum Task {
    Scope(usize, EffectNodeId),
    Emit(usize, EffectNodeId),
    Reduction(emit_reduction::Continuation),
    Otherwise {
        parent: usize,
        scope: usize,
        id: EffectNodeId,
        name: usize,
    },
    Join {
        parent: usize,
        scope: usize,
        id: EffectNodeId,
        name: usize,
    },
}

struct Emitter<'a> {
    program: &'a EffectProgram,
    language: Language,
    max_bytes: usize,
    limit: usize,
    body: String,
    helpers: BTreeMap<String, String>,
    helper_bytes: usize,
    scopes: Vec<Scope>,
    next_name: usize,
    samples_source: bool,
    reductions: HashMap<EffectNodeId, Option<reduction::Reduction>>,
    max_block_width: usize,
}

impl Emitter<'_> {
    fn reduction(&mut self, id: EffectNodeId) -> Option<reduction::Reduction> {
        let program = self.program;
        self.reductions
            .entry(id)
            .or_insert_with(|| reduction::Reduction::find(program, id))
            .clone()
    }
    fn lookup(&self, mut scope: usize, id: EffectNodeId) -> Option<usize> {
        loop {
            let current = &self.scopes[scope];
            if let Some(value) = current.values.get(&id) {
                return Some(*value);
            }
            if current.blocked.contains(&id.0) {
                return None;
            }
            scope = current.parent?;
        }
    }

    fn value(&self, scope: usize, id: EffectNodeId) -> String {
        format!("reel_n{}", self.lookup(scope, id).unwrap())
    }

    fn line(&mut self, scope: usize, text: &str) -> Result<(), GenerateError> {
        let indent = (self.scopes[scope].depth + 1) * 4;
        if self
            .body
            .len()
            .saturating_add(self.helper_bytes)
            .saturating_add(indent)
            .saturating_add(text.len() + 1)
            > self.max_bytes
        {
            return Err(GenerateError::SourceLimit { limit: self.limit });
        }
        self.body.extend(std::iter::repeat_n(' ', indent));
        self.body.push_str(text);
        self.body.push('\n');
        Ok(())
    }

    fn child(&mut self, parent: usize) -> usize {
        let index = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            depth: self.scopes[parent].depth + 1,
            values: HashMap::new(),
            blocked: BTreeSet::new(),
        });
        index
    }

    fn run(&mut self) -> Result<(), GenerateError> {
        let mut tasks = vec![Task::Scope(0, self.program.output())];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Scope(scope, output) => {
                    // A Select's arms belong to their selected child scope.
                    // Only its condition is an unconditional dependency here.
                    // Validated IR references point backwards, so sorted IDs
                    // form a topological schedule without moving branch-only
                    // samples or undefined arithmetic into their parent.
                    let mut reachable = BTreeSet::new();
                    let mut pending = vec![output];
                    while let Some(id) = pending.pop() {
                        if self.lookup(scope, id).is_some() || !reachable.insert(id.0) {
                            continue;
                        }
                        if let Some(reduction) = self.reduction(id) {
                            pending.extend(reduction.dependencies());
                            continue;
                        }
                        let op = &self.program.nodes()[id.0 as usize].op;
                        if let EffectOp::Select { condition, .. } = *op {
                            pending.push(condition);
                        } else {
                            pending.extend(dependencies(op));
                        }
                    }
                    tasks.extend(
                        reachable
                            .into_iter()
                            .rev()
                            .map(|id| Task::Emit(scope, EffectNodeId(id))),
                    );
                }
                Task::Emit(scope, id) => {
                    if self.lookup(scope, id).is_some() {
                        continue;
                    }
                    if let Some(reduction) = self.reduction(id) {
                        self.begin_reduction(scope, id, reduction, &mut tasks)?;
                        continue;
                    }
                    let node = self.program.nodes()[id.0 as usize].clone();
                    if let EffectOp::Select {
                        condition,
                        when_true,
                        ..
                    } = node.op
                    {
                        let name = self.next_name;
                        self.next_name += 1;
                        self.line(scope, &self.language.mutable(node.ty, name))?;
                        self.line(scope, &format!("if ({}) {{", self.value(scope, condition)))?;
                        let child = self.child(scope);
                        tasks.push(Task::Otherwise {
                            parent: scope,
                            scope: child,
                            id,
                            name,
                        });
                        tasks.push(Task::Scope(child, when_true));
                        continue;
                    }
                    let expression = self.expression(scope, node.ty, &node.op)?;
                    let name = self.next_name;
                    self.next_name += 1;
                    self.line(
                        scope,
                        &self.language.declaration(node.ty, name, &expression),
                    )?;
                    self.scopes[scope].values.insert(id, name);
                }
                Task::Reduction(continuation) => self.emit_reduction(continuation, &mut tasks)?,
                Task::Otherwise {
                    parent,
                    scope,
                    id,
                    name,
                } => {
                    let EffectOp::Select {
                        when_true,
                        when_false,
                        ..
                    } = self.program.nodes()[id.0 as usize].op
                    else {
                        unreachable!()
                    };
                    self.line(
                        scope,
                        &format!("reel_n{name} = {};", self.value(scope, when_true)),
                    )?;
                    self.line(parent, "} else {")?;
                    let scope = self.child(parent);
                    tasks.push(Task::Join {
                        parent,
                        scope,
                        id,
                        name,
                    });
                    tasks.push(Task::Scope(scope, when_false));
                }
                Task::Join {
                    parent,
                    scope,
                    id,
                    name,
                } => {
                    let EffectOp::Select { when_false, .. } =
                        self.program.nodes()[id.0 as usize].op
                    else {
                        unreachable!()
                    };
                    self.line(
                        scope,
                        &format!("reel_n{name} = {};", self.value(scope, when_false)),
                    )?;
                    self.line(parent, "}")?;
                    self.scopes[parent].values.insert(id, name);
                }
            }
        }
        Ok(())
    }

    fn expression(
        &mut self,
        scope: usize,
        ty: EffectType,
        op: &EffectOp,
    ) -> Result<String, GenerateError> {
        let value = |id| self.value(scope, id);
        Ok(match *op {
            EffectOp::Constant(value) => constant(self.language, value),
            EffectOp::Parameter(index) => {
                let name = match self.language {
                    Language::Cg => format!("reel_parameter_{index}"),
                    Language::Wgsl => format!("reel_parameters.p{index}"),
                };
                match ty {
                    EffectType::Bool => format!("({name}.x != 0.0)"),
                    EffectType::Float => format!("{name}.x"),
                    EffectType::Vec2 => format!("{name}.xy"),
                    EffectType::Vec3 => format!("{name}.xyz"),
                    EffectType::Vec4 => name,
                }
            }
            EffectOp::Input(input) => match input {
                EffectInput::Uv => "uv",
                EffectInput::Position => "position",
                EffectInput::Size => "size",
            }
            .into(),
            EffectOp::TextureSample {
                binding,
                uv,
                channel,
            } => {
                let suffix = match channel {
                    EffectTextureChannel::Color => "color",
                    EffectTextureChannel::Alpha => "alpha",
                    EffectTextureChannel::Gray => "gray",
                };
                format!("reel_texture_{binding}_{suffix}({})", value(uv))
            }
            EffectOp::SourceSample { uv } => {
                let uv = value(uv);
                self.samples_source = true;
                format!("reel_source_sample({uv})")
            }
            EffectOp::Vec2(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Vec3(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Vec4(ids) => construct(self.language, ty, ids.map(value).as_slice()),
            EffectOp::Extract { value: id, lane } => {
                format!("{}.{}", value(id), ["x", "y", "z", "w"][lane as usize])
            }
            EffectOp::Unary { op, value: id } => {
                let v = value(id);
                match op {
                    EffectUnary::Negate => format!("(-{v})"),
                    EffectUnary::Not => format!("(!{v})"),
                    EffectUnary::Fract if self.language == Language::Cg => format!("frac({v})"),
                    _ => format!("{}({v})", unary_name(op)),
                }
            }
            EffectOp::Binary { op, left, right } => {
                let a = value(left);
                let b = value(right);
                let operand_ty = self.program.nodes()[left.0 as usize].ty;
                match op {
                    EffectBinary::Remainder => {
                        let helper = self.helper("remainder", operand_ty)?;
                        format!("{helper}({a}, {b})")
                    }
                    EffectBinary::Equal | EffectBinary::NotEqual
                        if matches!(
                            operand_ty,
                            EffectType::Vec2 | EffectType::Vec3 | EffectType::Vec4
                        ) =>
                    {
                        let equal = op == EffectBinary::Equal;
                        format!(
                            "{}({a} {} {b})",
                            if equal { "all" } else { "any" },
                            if equal { "==" } else { "!=" }
                        )
                    }
                    EffectBinary::Atan2 => format!("atan2({a}, {b})"),
                    EffectBinary::Pow
                    | EffectBinary::Min
                    | EffectBinary::Max
                    | EffectBinary::Dot
                    | EffectBinary::Distance => format!("{}({a}, {b})", binary_name(op)),
                    _ => format!("({a} {} {b})", binary_name(op)),
                }
            }
            EffectOp::Ternary { op, a, b, c } => {
                let args = [value(a), value(b), value(c)];
                let name = match op {
                    EffectTernary::Clamp => "clamp".into(),
                    EffectTernary::Mix => if self.language == Language::Cg {
                        "lerp"
                    } else {
                        "mix"
                    }
                    .into(),
                    EffectTernary::Smoothstep => self.helper("smoothstep", ty)?,
                };
                format!("{name}({})", args.join(", "))
            }
            EffectOp::Select { .. } => unreachable!("selections are structured statements"),
        })
    }

    fn helper(&mut self, operation: &str, ty: EffectType) -> Result<String, GenerateError> {
        let suffix = match ty {
            EffectType::Float => "f",
            EffectType::Vec2 => "v2",
            EffectType::Vec3 => "v3",
            EffectType::Vec4 => "v4",
            EffectType::Bool => unreachable!(),
        };
        let name = format!("reel_{operation}_{suffix}");
        if !self.helpers.contains_key(&name) {
            let type_name = self.language.ty(ty);
            let mut source = match (self.language, operation) {
                (Language::Cg, "remainder") => format!(
                    "{type_name} {name}({type_name} a, {type_name} b) {{\n    {type_name} q = a / b;\n    {type_name} t = sign(q) * floor(abs(q));\n    return a - b * t;\n}}\n"
                ),
                (Language::Wgsl, "remainder") => format!(
                    "fn {name}(a: {type_name}, b: {type_name}) -> {type_name} {{\n    let q = a / b;\n    let t = sign(q) * floor(abs(q));\n    return a - b * t;\n}}\n"
                ),
                _ => String::new(),
            };
            if operation == "smoothstep" {
                if ty == EffectType::Float {
                    source = match self.language {
                        Language::Cg => format!(
                            "float {name}(float a, float b, float x) {{\n    if (a == b) return step(a, x);\n    float t = clamp((x - a) / (b - a), 0.0, 1.0);\n    return t * t * (3.0 - 2.0 * t);\n}}\n"
                        ),
                        Language::Wgsl => format!(
                            "fn {name}(a: f32, b: f32, x: f32) -> f32 {{\n    if (a == b) {{ return step(a, x); }}\n    let t = clamp((x - a) / (b - a), 0.0, 1.0);\n    return t * t * (3.0 - 2.0 * t);\n}}\n"
                        ),
                    };
                } else {
                    let scalar = self.helper(operation, EffectType::Float)?;
                    let lanes = match ty {
                        EffectType::Vec2 => 2,
                        EffectType::Vec3 => 3,
                        _ => 4,
                    };
                    let values = ["x", "y", "z", "w"][..lanes]
                        .iter()
                        .map(|lane| format!("{scalar}(a.{lane}, b.{lane}, x.{lane})"))
                        .collect::<Vec<_>>();
                    let result = construct(self.language, ty, &values);
                    source = match self.language {
                        Language::Cg => format!(
                            "{type_name} {name}({type_name} a, {type_name} b, {type_name} x) {{ return {result}; }}\n"
                        ),
                        Language::Wgsl => format!(
                            "fn {name}(a: {type_name}, b: {type_name}, x: {type_name}) -> {type_name} {{ return {result}; }}\n"
                        ),
                    };
                }
            }
            self.helper_bytes += source.len();
            if self.body.len().saturating_add(self.helper_bytes) > self.max_bytes {
                return Err(GenerateError::SourceLimit { limit: self.limit });
            }
            self.helpers.insert(name.clone(), source);
        }
        Ok(name)
    }
}

fn dependencies(op: &EffectOp) -> Vec<EffectNodeId> {
    match *op {
        EffectOp::Constant(_) | EffectOp::Parameter(_) | EffectOp::Input(_) => vec![],
        EffectOp::SourceSample { uv } | EffectOp::TextureSample { uv, .. } => vec![uv],
        EffectOp::Vec2(ids) => ids.to_vec(),
        EffectOp::Vec3(ids) => ids.to_vec(),
        EffectOp::Vec4(ids) => ids.to_vec(),
        EffectOp::Extract { value, .. } | EffectOp::Unary { value, .. } => vec![value],
        EffectOp::Binary { left, right, .. } => vec![left, right],
        EffectOp::Ternary { a, b, c, .. } => vec![a, b, c],
        EffectOp::Select { .. } => unreachable!(),
    }
}

fn construct(language: Language, ty: EffectType, values: &[String]) -> String {
    format!("{}({})", language.ty(ty), values.join(", "))
}

fn constant(language: Language, value: EffectValue) -> String {
    let number = |bits| {
        let value = f64::from(f32::from_bits(bits));
        if language == Language::Cg {
            // Sony Cg 2.2 mishandles integer-looking exponent literals such as
            // 1e0 in conditional expressions. A decimal mantissa is a float.
            format!("{value:.9e}")
        } else {
            format!("{value:e}")
        }
    };
    match value {
        EffectValue::Bool(value) => value.to_string(),
        EffectValue::Float(bits) => number(bits),
        EffectValue::Vec2(bits) => construct(language, EffectType::Vec2, &bits.map(number)),
        EffectValue::Vec3(bits) => construct(language, EffectType::Vec3, &bits.map(number)),
        EffectValue::Vec4(bits) => construct(language, EffectType::Vec4, &bits.map(number)),
    }
}

#[cfg(test)]
#[path = "effect_tests.rs"]
mod tests;

fn unary_name(op: EffectUnary) -> &'static str {
    match op {
        EffectUnary::Abs => "abs",
        EffectUnary::Floor => "floor",
        EffectUnary::Ceil => "ceil",
        EffectUnary::Fract => "fract",
        EffectUnary::Sin => "sin",
        EffectUnary::Cos => "cos",
        EffectUnary::Tan => "tan",
        EffectUnary::Asin => "asin",
        EffectUnary::Acos => "acos",
        EffectUnary::Atan => "atan",
        EffectUnary::Sqrt => "sqrt",
        EffectUnary::Exp => "exp",
        EffectUnary::Log => "log",
        EffectUnary::Exp2 => "exp2",
        EffectUnary::Log2 => "log2",
        EffectUnary::Length => "length",
        EffectUnary::Normalize => "normalize",
        _ => unreachable!(),
    }
}

fn binary_name(op: EffectBinary) -> &'static str {
    match op {
        EffectBinary::Add => "+",
        EffectBinary::Subtract => "-",
        EffectBinary::Multiply => "*",
        EffectBinary::Divide => "/",
        EffectBinary::Pow => "pow",
        EffectBinary::Min => "min",
        EffectBinary::Max => "max",
        EffectBinary::Dot => "dot",
        EffectBinary::Distance => "distance",
        EffectBinary::Equal => "==",
        EffectBinary::NotEqual => "!=",
        EffectBinary::Less => "<",
        EffectBinary::LessEqual => "<=",
        EffectBinary::Greater => ">",
        EffectBinary::GreaterEqual => ">=",
        _ => unreachable!(),
    }
}

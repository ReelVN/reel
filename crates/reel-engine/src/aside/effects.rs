//! Compiled effect identities and observable, typed instance parameters.
use super::{AdapterError, source, stage, text};
use aside_vm::bytecode::{BuiltinType, Const, Effects, FunctionId};
use aside_vm::{
    Heap, HeapError, HeapHandle, HeapReservation, ObjectKind, PropertyId, PropertyRead,
    SharedHeapReservation, Value, ValueTag,
};
use reel_ui::stage::effect::{EffectInstance, EffectType, EffectValue};
use std::sync::{Arc, Weak};

const TAG: i64 = -1111;
const TEXTURE_TAG: i64 = -1112;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Bind a pure drawing function to named parameters. The function receives a
        /// Fragment followed by typed Texture, float, bool or float-tuple parameters. Defaults
        /// come from its declaration; supplied values may be bind/derive references.
        /// Assign the result to effect, or a float result to mask. The compiler prepares the
        /// drawing program; parameter changes reuse it across frames and nodes.
        /// A float result supplies coverage; a four-float tuple supplies linear
        /// premultiplied RGBA. Functions must be statically resolved without captures.
        /// Drawing arithmetic uses 32-bit floats. Effects cover the node's logical
        /// rectangle and run before its invert, opacity, mask and blend properties.
        ///
        /// ```aside
        /// use reel::types::Fragment;
        /// fn tint(pixel: Fragment, amount: float = 0.5) -> (float, float, float, float) {
        ///     let (r, g, b, a) = pixel.sample();
        ///     (r * amount, g, b, a)
        /// }
        /// fn main() {
        ///     let state = model({amount: 0.5});
        ///     let paint = effect(tint, params: {amount: bind(state.amount)});
        ///     stage.replace(stack(width: 320, height: 180,
        ///         background: "#ffffff", effect: paint));
        ///     state.amount = 0.8;
        /// }
        /// ```
        "reel::effect";
        /// Create an immutable whole-image source for a typed Texture parameter.
        /// The path uses the ordinary project resource namespace and target rules.
        /// Loading begins when a visible effect needs it; prepared textures are accepted.
        /// A Texture parameter accepts this value and observable bindings to it.
        ///
        /// ```aside
        /// use reel::types::{Fragment, Texture};
        /// fn dissolve(pixel: Fragment, noise: Texture, progress: float) -> float {
        ///     if noise.gray(pixel.uv) <= progress { 1.0 } else { 0.0 }
        /// }
        /// fn main() {
        ///     let scene = stack(width: 640, height: 360,
        ///         background: "#ffffff", mask_progress: 0.25);
        ///     scene.mask = effect(dissolve, params: {
        ///         noise: texture("assets/noise.png"), progress: bind(scene.mask_progress)
        ///     });
        ///     stage.replace(scene);
        ///     scene.mask_progress = 0.75;
        /// }
        /// ```
        "reel::texture";
        /// Sample a whole image in linear premultiplied RGBA at normalized uv.
        /// Coordinates clamp to content texel centers. Available in compiled effects.
        "reel::texture::sample";
        /// Sample image alpha at normalized uv, using the same image rules as masks.
        "reel::texture::alpha";
        /// Sample the mask grayscale channel after filtering and color decoding.
        "reel::texture::gray";
        /// Sample the completed subtree in linear premultiplied RGBA. Omitted uv
        /// selects the current fragment. Coordinates are normalized to the node's
        /// logical rectangle and clamp to its edges. Available within compiled
        /// drawing functions.
        "reel::fragment::sample";
    }
}

pub(super) fn contracts() -> Vec<aside_host::NativeContract> {
    use aside_host::{NativeContract, NativeParameter};
    let mut declarations = vec![
        (
            "reel::texture",
            vec![("path", BuiltinType::String, None)],
            BuiltinType::Any,
            Effects::ALLOCATE,
        ),
        (
            "reel::effect",
            vec![
                ("function", BuiltinType::Closure, None),
                ("params", BuiltinType::Any, Some(Const::Null)),
            ],
            BuiltinType::Any,
            Effects::ALLOCATE,
        ),
        (
            "reel::fragment::sample",
            vec![
                ("receiver", BuiltinType::Any, None),
                ("uv", BuiltinType::Any, Some(Const::Null)),
            ],
            BuiltinType::Tuple,
            Effects::ALLOCATE,
        ),
    ];
    for method in ["sample", "alpha", "gray"] {
        declarations.push((
            match method {
                "sample" => "reel::texture::sample",
                "alpha" => "reel::texture::alpha",
                _ => "reel::texture::gray",
            },
            vec![
                ("receiver", BuiltinType::Any, None),
                ("uv", BuiltinType::Tuple, None),
            ],
            if method == "sample" {
                BuiltinType::Tuple
            } else {
                BuiltinType::Float
            },
            Effects::ALLOCATE,
        ));
    }
    declarations
        .into_iter()
        .map(|(symbol, parameters, result, effects)| NativeContract {
            symbol: symbol.into(),
            abi_version: 1,
            parameters: parameters
                .into_iter()
                .map(|(name, ty, default)| NativeParameter {
                    name: name.into(),
                    ty,
                    default,
                })
                .collect(),
            rest: None,
            result,
            effects: Effects(effects),
            required_capabilities: vec![],
        })
        .collect()
}

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, nullable};
    api.refine(
        "reel::texture",
        named("reel::types::Texture"),
        &[("path", TypeHint::String)],
    );
    let mut texture_members = Vec::new();
    for method in ["sample", "alpha", "gray"] {
        let symbol = format!("reel::texture::{method}");
        api.refine(
            &symbol,
            if method == "sample" {
                TypeHint::Tuple(vec![TypeHint::Float; 4])
            } else {
                TypeHint::Float
            },
            &[
                ("receiver", named("reel::types::Texture")),
                ("uv", TypeHint::Tuple(vec![TypeHint::Float; 2])),
            ],
        );
        texture_members.push(Member::method(api.native(&symbol).clone().bound(method)));
    }
    api.types.push(NamedType::new(
        "reel::types::Texture",
        "An immutable whole-image source bound to a compiled drawing function.",
        texture_members,
    ));
    api.refine(
        "reel::effect",
        named("reel::types::Effect"),
        &[
            ("function", TypeHint::Closure),
            ("params", nullable(TypeHint::Table)),
        ],
    );
    api.refine(
        "reel::fragment::sample",
        TypeHint::Tuple(vec![TypeHint::Float; 4]),
        &[
            ("receiver", named("reel::types::Fragment")),
            ("uv", nullable(TypeHint::Tuple(vec![TypeHint::Float; 2]))),
        ],
    );
    let mut members = Vec::new();
    for (name, doc) in [
        (
            "uv",
            "Normalized coordinates in this node's logical rectangle.",
        ),
        ("position", "Coordinates in this node's logical pixels."),
        ("size", "Width and height of this node's logical rectangle."),
    ] {
        members.push(Member::field(
            "reel::types::Fragment",
            name,
            TypeHint::Tuple(vec![TypeHint::Float; 2]),
            None,
            doc,
            None,
        ));
    }
    members.push(Member::method(
        api.native("reel::fragment::sample").clone().bound("sample"),
    ));
    api.types.push(NamedType::new(
        "reel::types::Fragment",
        "Drawing context supplied to a compiled effect function.",
        members,
    ));
    api.types.push(NamedType::new(
        "reel::types::Effect",
        "A compiled drawing function with bound parameters, shared by stage nodes.",
        Vec::new(),
    ));
}

pub(super) fn create_texture(
    heap: &mut Heap,
    path: Value,
    limit: usize,
) -> Result<Value, AdapterError> {
    let info = heap.info(path)?;
    if info.kind != ObjectKind::String || info.len == 0 || info.len as usize > limit {
        return Err(AdapterError::Input(
            "texture requires a nonempty image asset path",
        ));
    }
    let mut suffix = [0; 11];
    if info.len >= 11 {
        heap.read_bytes(path, info.len - 11, &mut suffix)?;
        if &suffix == b".reel-image" {
            return Err(AdapterError::Input(
                "texture requires a whole image or prepared texture",
            ));
        }
    }
    let fields = heap.alloc_table()?;
    Ok(heap.alloc_immutable_host(fields, &[Value::int(TEXTURE_TAG), path])?)
}
fn validate_texture(heap: &Heap, input: Value) -> Result<(), HeapError> {
    if heap.info(input)?.kind == ObjectKind::Host
        && heap.host_payload(input, 0)?.as_int() == Some(TEXTURE_TAG)
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
pub(super) fn is_effect(heap: &Heap, input: Value) -> bool {
    input.tag() != ValueTag::Null && validate(heap, input).is_ok()
}
pub(super) fn validate_mask(heap: &Heap, input: Value) -> Result<(), HeapError> {
    validate(heap, input)?;
    if heap
        .host_payload(input, heap.host_payload_len(input)? - 1)?
        .as_int()
        == Some(1)
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

fn scalar(value: Value) -> Result<u32, HeapError> {
    let number = stage::number(value).map_err(|_| HeapError::TypeMismatch)? as f32;
    number
        .is_finite()
        .then_some(number.to_bits())
        .ok_or(HeapError::TypeMismatch)
}

fn value(heap: &Heap, ty: EffectType, value: Value) -> Result<EffectValue, HeapError> {
    Ok(match ty {
        EffectType::Bool => EffectValue::Bool(value.as_bool().ok_or(HeapError::TypeMismatch)?),
        EffectType::Float => EffectValue::Float(scalar(value)?),
        EffectType::Vec2 | EffectType::Vec3 | EffectType::Vec4 => {
            let len = match ty {
                EffectType::Vec2 => 2,
                EffectType::Vec3 => 3,
                _ => 4,
            };
            if heap.info(value)?.kind != ObjectKind::Tuple || heap.sequence_len(value)? != len {
                return Err(HeapError::TypeMismatch);
            }
            let mut bits = [0; 4];
            for (i, bit) in bits.iter_mut().take(len as usize).enumerate() {
                *bit = scalar(heap.sequence_get(value, i as u32)?)?;
            }
            match ty {
                EffectType::Vec2 => EffectValue::Vec2([bits[0], bits[1]]),
                EffectType::Vec3 => EffectValue::Vec3([bits[0], bits[1], bits[2]]),
                _ => EffectValue::Vec4(bits),
            }
        }
    })
}

fn default_value(heap: &mut Heap, value: EffectValue) -> Result<Value, HeapError> {
    let component = |bits| Value::float(f64::from(f32::from_bits(bits)));
    Ok(match value {
        EffectValue::Bool(v) => Value::bool(v),
        EffectValue::Float(v) => component(v),
        EffectValue::Vec2(v) => heap.alloc_tuple(&v.map(component))?,
        EffectValue::Vec3(v) => heap.alloc_tuple(&v.map(component))?,
        EffectValue::Vec4(v) => heap.alloc_tuple(&v.map(component))?,
    })
}

fn type_code(ty: EffectType) -> i64 {
    match ty {
        EffectType::Bool => 0,
        EffectType::Float => 1,
        EffectType::Vec2 => 2,
        EffectType::Vec3 => 3,
        EffectType::Vec4 => 4,
    }
}

pub(super) static PARAMETER: aside_vm::HostPropertyValidator =
    aside_vm::HostPropertyValidator::new("reel.effect", 1, validate_parameter);

pub(super) fn validate_parameter(
    read: &PropertyRead<'_>,
    host: Value,
    field: u32,
    proposed: Value,
) -> Result<(), HeapError> {
    if read.host_payload(host, 2 + field)?.as_int() == Some(5) {
        return validate_texture(read, proposed);
    }
    let ty = match read.host_payload(host, 2 + field)?.as_int() {
        Some(0) => EffectType::Bool,
        Some(1) => EffectType::Float,
        Some(2) => EffectType::Vec2,
        Some(3) => EffectType::Vec3,
        Some(4) => EffectType::Vec4,
        _ => return Err(HeapError::TypeMismatch),
    };
    value(read, ty, proposed).map(|_| ())
}

pub(super) fn create(
    heap: &mut Heap,
    function: FunctionId,
    definition: &source::EffectDefinition,
    params: Value,
) -> Result<Value, AdapterError> {
    let schema = definition.program.parameters();
    let textures = definition.program.textures();
    let names: Vec<&str> = schema
        .iter()
        .map(|p| p.name.as_ref())
        .chain(textures.iter().map(|p| p.name.as_ref()))
        .collect();
    let mut supplied = vec![None; names.len()];
    if params.tag() != ValueTag::Null {
        for i in 0..heap.table_len(params)? {
            let (key, input) = heap.table_entry(params, i)?;
            let key = text::read_string(heap, key, 4096)?;
            let index = names
                .iter()
                .position(|name| *name == key)
                .ok_or(AdapterError::Input("unknown effect parameter"))?;
            supplied[index] = Some(input);
        }
    }
    let fields = heap.alloc_table()?;
    let mut payload = Vec::with_capacity(names.len() + 2);
    payload.extend([Value::int(TAG), Value::int(function.0 as i64)]);
    for (index, name) in names.iter().enumerate() {
        let input = match supplied[index] {
            Some(input) => heap.property_argument(input)?,
            None => default_value(
                heap,
                schema
                    .get(index)
                    .and_then(|p| p.default)
                    .ok_or(AdapterError::Input("missing required effect parameter"))?,
            )?,
        };
        let kind = if let Some(parameter) = schema.get(index) {
            value(heap, parameter.ty, input)?;
            type_code(parameter.ty)
        } else {
            validate_texture(heap, input)?;
            5
        };
        let key = heap.alloc_string(name)?;
        heap.table_store(fields, index as u32, key, input)?;
        payload.push(Value::int(kind));
    }
    payload.push(Value::int(type_code(
        definition.program.nodes()[definition.program.output().0 as usize].ty,
    )));
    let effect = heap.alloc_host(fields, &payload, Some(&PARAMETER))?;
    heap.observe_host_properties(effect, names.len() as u32, 0)?;
    for (index, input) in supplied.into_iter().enumerate() {
        if let Some(input) = input
            && heap.is_property_ref(input)
        {
            heap.bind_host_property(
                PropertyId {
                    host: effect,
                    field: index as u32,
                },
                input,
            )?;
        }
    }
    Ok(effect)
}

pub(super) fn validate(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null
        || (heap.info(value)?.kind == ObjectKind::Host
            && heap.host_payload(value, 0)?.as_int() == Some(TAG))
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

/// A weak identity and parameter revision allow unchanged nodes to skip projection.
pub(super) fn stamp(
    heap: &Heap,
    value: Value,
) -> Result<Option<(aside_vm::HeapHandle, u64)>, HeapError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    validate(heap, value)?;
    Ok(Some((
        heap.handle(value)?,
        heap.info(heap.host_fields(value)?)?.content_version,
    )))
}

pub(super) fn changed(
    heap: &Heap,
    stamp: Option<(aside_vm::HeapHandle, u64)>,
) -> Result<bool, HeapError> {
    stamp
        .map(|(handle, version)| {
            let fields = heap.host_fields(heap.import_handle(handle)?)?;
            Ok(heap.info(fields)?.content_version != version)
        })
        .transpose()
        .map(|changed| changed.unwrap_or(false))
}

struct AdmittedDefinition {
    definition: Arc<source::EffectDefinition>,
    _charge: SharedHeapReservation,
}
struct ParameterValues {
    values: Vec<EffectValue>,
    _definition: Arc<AdmittedDefinition>,
    _charge: SharedHeapReservation,
}
impl AsRef<[EffectValue]> for ParameterValues {
    fn as_ref(&self) -> &[EffectValue] {
        &self.values
    }
}
struct CachedInstance {
    // Drop the weak allocation before the owner refunds its complete storage.
    instance: Weak<EffectInstance>,
    _values: Arc<ParameterValues>,
}
type InstanceKey = (HeapHandle, u64);

/// Heap-local weak identities; immutable allocations carry their own sendable
/// admission owner when UI snapshots outlive the VM that prepared them.
#[derive(Default)]
pub(super) struct ProjectionCache {
    definitions: Vec<Arc<AdmittedDefinition>>,
    instances: hashbrown::HashMap<InstanceKey, CachedInstance>,
    _products_charge: Option<HeapReservation>,
    cache_charge: Option<HeapReservation>,
}
impl ProjectionCache {
    pub fn admit(heap: &Heap, products: &source::EffectProducts) -> Result<Self, AdapterError> {
        if products.is_empty() {
            return Ok(Self::default());
        }
        let definitions = products.definitions();
        let count = definitions.len();
        let definition_bytes: usize = products.definitions().map(|d| d.storage_bytes()).sum();
        let container_bytes = products.storage_bytes() - definition_bytes;
        let mut charge = heap
            .budget_usage()
            .reserve((container_bytes + count * size_of::<Arc<AdmittedDefinition>>()) as u64)?;
        let mut admitted = Vec::new();
        admitted
            .try_reserve_exact(count)
            .map_err(|_| AdapterError::Allocation)?;
        charge.resize(
            (container_bytes + admitted.capacity() * size_of::<Arc<AdmittedDefinition>>()) as u64,
        )?;
        for definition in definitions {
            let bytes = definition.storage_bytes()
                + size_of::<AdmittedDefinition>()
                + 2 * size_of::<usize>();
            let reservation = heap.budget_usage().reserve_shared(bytes as u64)?;
            admitted.push(Arc::new(AdmittedDefinition {
                definition: definition.clone(),
                _charge: reservation,
            }));
        }
        Ok(Self {
            definitions: admitted,
            _products_charge: Some(charge),
            ..Self::default()
        })
    }

    /// Reclaim retired parameter revisions once per synchronization batch.
    pub fn collect(&mut self) {
        if self.instances.capacity() == 0 {
            return;
        }
        self.instances
            .retain(|_, entry| entry.instance.strong_count() != 0);
        if self.instances.is_empty() {
            self.instances = hashbrown::HashMap::new();
            self.cache_charge = None;
        }
    }

    fn reserve_entry(&mut self, heap: &Heap) -> Result<(), AdapterError> {
        if self.instances.len() < self.instances.capacity() {
            return Ok(());
        }
        let capacity = self
            .instances
            .len()
            .max(4)
            .checked_mul(2)
            .ok_or(AdapterError::Allocation)?;
        // Reserve a new table alongside the old allocation, then retain its
        // reported bucket/control capacity after the pinned allocator admits it.
        let upper = capacity
            .checked_mul(4 * (size_of::<(InstanceKey, CachedInstance)>() + 16))
            .ok_or(AdapterError::Allocation)?;
        let mut charge = heap.budget_usage().reserve(upper as u64)?;
        let mut next = hashbrown::HashMap::new();
        next.try_reserve(capacity)
            .map_err(|_| AdapterError::Allocation)?;
        charge.resize(next.allocation_size() as u64)?;
        next.extend(self.instances.drain());
        self.instances = next;
        self.cache_charge = Some(charge);
        Ok(())
    }

    pub fn project(
        &mut self,
        heap: &Heap,
        host: Value,
        products: &source::EffectProducts,
    ) -> Result<Option<Arc<EffectInstance>>, AdapterError> {
        let Some(key) = stamp(heap, host)? else {
            return Ok(None);
        };
        if let Some(instance) = self
            .instances
            .get(&key)
            .and_then(|entry| entry.instance.upgrade())
        {
            return Ok(Some(instance));
        }
        let function = FunctionId(heap.host_payload(host, 1)?.as_int().unwrap() as u32);
        let definition = products
            .get(function)
            .ok_or(AdapterError::Input("missing effect definition"))?;
        let index = self
            .definitions
            .binary_search_by_key(&definition.id, |entry| entry.definition.id)
            .map_err(|_| AdapterError::Input("effect definition was not admitted"))?;
        let schema = definition.program.parameters();
        self.reserve_entry(heap)?;
        let instance_bytes = size_of::<EffectInstance>() + 2 * size_of::<usize>();
        let owner_bytes = size_of::<ParameterValues>() + 2 * size_of::<usize>();
        let mut charge = heap.budget_usage().reserve(
            (instance_bytes + owner_bytes + schema.len() * size_of::<EffectValue>()) as u64,
        )?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(schema.len())
            .map_err(|_| AdapterError::Allocation)?;
        let storage = owner_bytes + values.capacity() * size_of::<EffectValue>();
        charge.resize((instance_bytes + storage) as u64)?;
        let fields = heap.host_fields(host)?;
        for (index, parameter) in schema.iter().enumerate() {
            values.push(value(
                heap,
                parameter.ty,
                heap.table_entry(fields, index as u32)?.1,
            )?);
        }
        let mut textures = Vec::new();
        charge.resize(
            charge.bytes() + (definition.program.textures().len() * size_of::<Arc<str>>()) as u64,
        )?;
        textures
            .try_reserve_exact(definition.program.textures().len())
            .map_err(|_| AdapterError::Allocation)?;
        for slot in 0..definition.program.textures().len() {
            let host = heap.table_entry(fields, (schema.len() + slot) as u32)?.1;
            validate_texture(heap, host)?;
            let path = heap.host_payload(host, 1)?;
            let path_len = heap.info(path)?.len as u64;
            charge.resize(charge.bytes() + 2 * path_len + (2 * size_of::<usize>()) as u64)?;
            textures.push(Arc::from(text::read_string(heap, path, usize::MAX)?));
            charge.resize(charge.bytes() - path_len)?;
        }
        let values = Arc::new(ParameterValues {
            values,
            _definition: self.definitions[index].clone(),
            _charge: charge.into_shared()?,
        });
        let instance = EffectInstance::from_owned_inputs(
            definition.id,
            definition.program.clone(),
            reel_ui::peniko::Blob::new(values.clone()),
            storage,
            textures,
        )
        .map_err(|_| AdapterError::Input("invalid effect parameters"))?;
        self.instances.insert(
            key,
            CachedInstance {
                instance: Arc::downgrade(&instance),
                _values: values,
            },
        );
        Ok(Some(instance))
    }
    /// Bind a proven constructor graph through the same immutable definition and
    /// parameter owner as explicit effects. Animation keeps the selected definition.
    pub fn project_mask(
        &mut self,
        heap: &Heap,
        mask: &reel_ui::stage::MaskProgram,
        parameters: [f32; 4],
        products: &source::EffectProducts,
        previous: Option<&Arc<EffectInstance>>,
    ) -> Result<Option<Arc<EffectInstance>>, AdapterError> {
        let definition = if let Some(previous) = previous {
            products.by_id(previous.definition())
        } else {
            products.mask_definition(mask)
        };
        let Some(definition) = definition else {
            return Ok(None);
        };
        if let Some(previous) = previous
            && previous
                .values()
                .iter()
                .zip(parameters)
                .all(|(value, parameter)| *value == EffectValue::Float(parameter.to_bits()))
            && previous.textures() == mask.sources()
        {
            return Ok(Some(previous.clone()));
        }
        let index = self
            .definitions
            .binary_search_by_key(&definition.id, |entry| entry.definition.id)
            .map_err(|_| AdapterError::Input("mask definition was not admitted"))?;
        let owner_bytes = size_of::<ParameterValues>() + 2 * size_of::<usize>();
        let storage = owner_bytes + 4 * size_of::<EffectValue>();
        let bytes = size_of::<EffectInstance>()
            + 2 * size_of::<usize>()
            + storage
            + std::mem::size_of_val(mask.sources())
            + mask
                .sources()
                .iter()
                .map(|path| path.len() + 2 * size_of::<usize>())
                .sum::<usize>();
        let charge = heap.budget_usage().reserve_shared(bytes as u64)?;
        let values = Arc::new(ParameterValues {
            values: parameters.map(|v| EffectValue::Float(v.to_bits())).to_vec(),
            _definition: self.definitions[index].clone(),
            _charge: charge,
        });
        let textures = mask
            .sources()
            .iter()
            .map(|path| Arc::from(path.as_ref()))
            .collect();
        EffectInstance::from_owned_inputs(
            definition.id,
            definition.program.clone(),
            reel_ui::peniko::Blob::new(values),
            storage,
            textures,
        )
        .map(Some)
        .map_err(|_| AdapterError::Input("invalid static mask parameters"))
    }
}

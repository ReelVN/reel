//! Immutable scalar mask expressions; the VM owns the complete shared graph.
use super::{AdapterError, stage, text};

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{NamedType, named, number};
    let mask = named("reel::types::Mask");
    let scalar = TypeHint::Union(vec![mask.clone(), number()]);
    for contract in contracts() {
        let hints = contract
            .parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.name.as_str(),
                    match parameter.name.as_str() {
                        "source" | "repeat" | "name" => TypeHint::String,
                        "power" => number(),
                        _ => scalar.clone(),
                    },
                )
            })
            .collect::<Vec<_>>();
        api.refine(&contract.symbol, mask.clone(), &hints);
    }
    api.types.push(NamedType::new(
        "reel::types::Mask",
        "An immutable scalar mask expression sampled by stage appearance and transitions.",
        Vec::new(),
    ));
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create an immutable mask expression sampling image alpha. x/y and scale_x/y
        /// transform sampling; repeat accepts none, x, y or both. Source is a whole
        /// PNG, JPEG or prepared texture.
        "reel::mask::alpha";
        /// Create an immutable mask expression sampling image grayscale. Sampling
        /// transform and repetition follow mask.alpha.
        "reel::mask::gray";
        /// Read the mask's live progress, softness, x or y parameter inside an immutable
        /// expression. Bind or animate the corresponding node property to vary it.
        "reel::mask::param";
        /// Add two scalar mask expressions or numeric constants.
        "reel::mask::add";
        /// Subtract the second mask expression or number from the first.
        "reel::mask::subtract";
        /// Multiply two scalar mask expressions or numeric constants.
        "reel::mask::multiply";
        /// Raise a scalar mask expression to a finite nonnegative power.
        "reel::mask::pow";
        /// Clamp a scalar mask expression to min/max, defaulting to zero and one.
        "reel::mask::clamp";
        /// Apply smoothstep between edge0 and edge1 to a scalar mask expression.
        "reel::mask::smoothstep";
    }
}
use aside_vm::{Heap, HeapError, HeapHandle, ObjectKind, Value};
use reel_ui::stage::{MaskChannel, MaskOp, MaskProgram};
use std::{collections::HashMap, sync::Arc};

const TAG: i64 = -1109;

fn scalar(value: Value) -> Result<f32, AdapterError> {
    let number = stage::number(value)? as f32;
    if !number.is_finite() {
        return Err(AdapterError::Input(
            "mask number must be representable as finite f32",
        ));
    }
    Ok(number)
}

pub(super) fn validate(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if scalar(value).is_ok() {
        return Ok(());
    }
    if heap.info(value)?.kind == ObjectKind::Host
        && heap.host_payload(value, 0)?.as_int() == Some(TAG)
    {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

pub(super) fn create(
    heap: &mut Heap,
    name: &str,
    args: &[Value],
    limit: usize,
) -> Result<Value, AdapterError> {
    let mut values = args.to_vec();
    let opcode = match name {
        "alpha" | "gray" => {
            let source = text::read_string(heap, args[0], limit)?;
            if source.is_empty() || source.ends_with(".reel-image") {
                return Err(AdapterError::Input(
                    "mask source must be a whole image or prepared texture",
                ));
            }
            for &value in &args[1..5] {
                validate(heap, value)?;
            }
            let repeat = match text::read_string(heap, args[5], limit)?.as_str() {
                "none" => 0,
                "x" => 1,
                "y" => 2,
                "both" => 3,
                _ => {
                    return Err(AdapterError::Input(
                        "mask repeat must be none, x, y or both",
                    ));
                }
            };
            values[5] = Value::int(repeat);
            if name == "alpha" { 0 } else { 1 }
        }
        "param" => {
            let index = match text::read_string(heap, args[0], limit)?.as_str() {
                "progress" => 0,
                "softness" => 1,
                "x" => 2,
                "y" => 3,
                _ => {
                    return Err(AdapterError::Input(
                        "mask parameter must be progress, softness, x or y",
                    ));
                }
            };
            values[0] = Value::int(index);
            2
        }
        "add" | "subtract" | "multiply" => {
            for &value in args {
                validate(heap, value)?;
            }
            match name {
                "add" => 3,
                "subtract" => 4,
                _ => 5,
            }
        }
        "pow" => {
            validate(heap, args[0])?;
            if scalar(args[1])? < 0. {
                return Err(AdapterError::Input("mask power must be nonnegative"));
            }
            6
        }
        "clamp" => {
            validate(heap, args[0])?;
            if scalar(args[1])? > scalar(args[2])? {
                return Err(AdapterError::Input("mask clamp minimum exceeds maximum"));
            }
            7
        }
        "smoothstep" => {
            for &value in args {
                validate(heap, value)?;
            }
            8
        }
        _ => unreachable!("registered mask constructor"),
    };
    let fields = heap.alloc_table()?;
    let mut payload = vec![Value::int(TAG), Value::int(opcode)];
    payload.extend(values);
    Ok(heap.alloc_immutable_host(fields, &payload)?)
}

pub(super) fn project(
    heap: &Heap,
    root: Value,
    limit: usize,
) -> Result<Arc<MaskProgram>, AdapterError> {
    if heap.info(root)?.kind == ObjectKind::String {
        let _charge = heap.budget_usage().reserve(
            heap.info(root)?.len as u64
                + (size_of::<MaskProgram>() + 5 * size_of::<MaskOp>() + size_of::<Arc<str>>())
                    as u64,
        )?;
        let source = text::read_string(heap, root, limit)?;
        if source.is_empty() || source.ends_with(".reel-image") {
            return Err(AdapterError::Input(
                "mask source must be a whole image or prepared texture",
            ));
        }
        return Ok(MaskProgram::image(Arc::from(source)));
    }
    let mut charge = heap.budget_usage().reserve(
        (size_of::<Vec<MaskOp>>()
            + size_of::<Vec<Arc<str>>>()
            + size_of::<HashMap<HeapHandle, u32>>()
            + size_of::<HashMap<u32, u32>>()
            + size_of::<Vec<(Value, bool)>>()
            + size_of::<(Value, bool)>()) as u64,
    )?;
    let mut operations = Vec::new();
    let mut sources = Vec::<Arc<str>>::new();
    let mut nodes = HashMap::<HeapHandle, u32>::new();
    let mut constants = HashMap::<u32, u32>::new();
    let mut pending = vec![(root, false)];
    while let Some((value, ready)) = pending.pop() {
        if let Ok(number) = scalar(value) {
            if !constants.contains_key(&number.to_bits()) {
                charge.resize(
                    charge.bytes()
                        + (4 * (size_of::<MaskOp>() + size_of::<(u32, u32)>() + 16)) as u64,
                )?;
            }
            constants.entry(number.to_bits()).or_insert_with(|| {
                let index = operations.len() as u32;
                operations.push(MaskOp::Constant(number.to_bits()));
                index
            });
            continue;
        }
        let handle = heap.handle(value)?;
        if nodes.contains_key(&handle) {
            continue;
        }
        let opcode = heap.host_payload(value, 1)?.as_int().unwrap();
        let count = heap.host_payload_len(value)?;
        let args = (2..count)
            .map(|i| heap.host_payload(value, i))
            .collect::<Result<Vec<_>, _>>()?;
        let children = match opcode {
            0 | 1 => &args[1..5],
            2 => &[],
            6 | 7 => &args[..1],
            _ => &args[..],
        };
        if !ready {
            // Charge vector growth, old allocations during growth, hash buckets and
            // the bounded operand work stack against the VM's existing heap budget.
            let overhead = 4
                * (size_of::<MaskOp>()
                    + size_of::<(HeapHandle, u32)>()
                    + size_of::<Arc<str>>()
                    + 6 * size_of::<(Value, bool)>()
                    + 32);
            charge.resize(charge.bytes() + overhead as u64)?;
            pending.push((value, true));
            pending.extend(children.iter().rev().map(|&child| (child, false)));
            continue;
        }
        let index = |value: Value| -> Result<u32, AdapterError> {
            Ok(if let Ok(number) = scalar(value) {
                constants[&number.to_bits()]
            } else {
                nodes[&heap.handle(value)?]
            })
        };
        let operation = match opcode {
            0 | 1 => {
                charge.resize(charge.bytes() + heap.info(args[0])?.len as u64)?;
                let source: Arc<str> = text::read_string(heap, args[0], limit)?.into();
                let image = if let Some(index) = sources.iter().position(|s| *s == source) {
                    index
                } else {
                    sources.push(source);
                    sources.len() - 1
                } as u32;
                MaskOp::Sample {
                    image,
                    channel: if opcode == 0 {
                        MaskChannel::Alpha
                    } else {
                        MaskChannel::Gray
                    },
                    coordinates: [
                        index(args[1])?,
                        index(args[2])?,
                        index(args[3])?,
                        index(args[4])?,
                    ],
                    repeat: args[5].as_int().unwrap() as u8,
                }
            }
            2 => MaskOp::Parameter(args[0].as_int().unwrap() as u8),
            3 => MaskOp::Add(index(args[0])?, index(args[1])?),
            4 => MaskOp::Subtract(index(args[0])?, index(args[1])?),
            5 => MaskOp::Multiply(index(args[0])?, index(args[1])?),
            6 => MaskOp::Pow(index(args[0])?, scalar(args[1])?.to_bits()),
            7 => MaskOp::Clamp(
                index(args[0])?,
                scalar(args[1])?.to_bits(),
                scalar(args[2])?.to_bits(),
            ),
            8 => MaskOp::Smoothstep(index(args[0])?, index(args[1])?, index(args[2])?),
            _ => unreachable!("validated mask expression"),
        };
        nodes.insert(handle, operations.len() as u32);
        operations.push(operation);
    }
    MaskProgram::new(operations, sources).map_err(AdapterError::Input)
}

pub(super) fn contracts() -> Vec<aside_host::NativeContract> {
    use aside_vm::bytecode::{BuiltinType, Const, Effects};
    let mut result = Vec::new();
    for name in [
        "alpha",
        "gray",
        "param",
        "add",
        "subtract",
        "multiply",
        "pow",
        "clamp",
        "smoothstep",
    ] {
        let fields: Vec<(&str, Option<Const>)> = match name {
            "alpha" | "gray" => vec![
                ("source", None),
                ("x", Some(Const::Int(0))),
                ("y", Some(Const::Int(0))),
                ("scale_x", Some(Const::Int(1))),
                ("scale_y", Some(Const::Int(1))),
                ("repeat", Some(Const::Utf8("none".into()))),
            ],
            "param" => vec![("name", None)],
            "pow" => vec![("value", None), ("power", None)],
            "clamp" => vec![
                ("value", None),
                ("min", Some(Const::Int(0))),
                ("max", Some(Const::Int(1))),
            ],
            "smoothstep" => vec![("edge0", None), ("edge1", None), ("value", None)],
            _ => vec![("a", None), ("b", None)],
        };
        result.push(aside_host::NativeContract {
            standard_derived_commit: false,
            read_only_leaf: None,
            symbol: format!("reel::mask::{name}"),
            abi_version: 1,
            parameters: fields
                .into_iter()
                .map(|(name, default)| aside_host::NativeParameter {
                    name: name.into(),
                    ty: BuiltinType::Any,
                    default,
                })
                .collect(),
            rest: None,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE),
            required_capabilities: vec![],
        });
    }
    result
}

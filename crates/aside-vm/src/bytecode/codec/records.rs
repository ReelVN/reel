//! Wire records for function, source, type and descriptor metadata.
use super::*;

impl Wire for FunctionKind {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Plain => 0u8.put(w),
            Self::Async => 1u8.put(w),
            Self::Generator => 2u8.put(w),
            Self::Native(id) => {
                3u8.put(w)?;
                id.put(w)
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Plain),
            1 => Ok(Self::Async),
            2 => Ok(Self::Generator),
            3 => Ok(Self::Native(Wire::get(r)?)),
            _ => Err(r.error(ErrorKind::Encoding, "unknown FunctionKind tag")),
        }
    }
}
unit_enum!(Context{0=>Ordinary,1=>NoWait,2=>Derived,3=>Init});
unit_enum!(CaptureKind{0=>Value,1=>Cell});
tuple_enum!(BindingStorage{0=>Value(reg),1=>Cell(reg)});
impl Wire for BindingInitialization {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Explicit => 0u8.put(w),
            Self::Parameter(i) => {
                1u8.put(w)?;
                i.put(w)
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Explicit),
            1 => Ok(Self::Parameter(u32::get(r)?)),
            _ => Err(r.error(ErrorKind::Encoding, "binding initialization tag")),
        }
    }
}
tuple_enum!(CaptureSource{0=>Binding(id),1=>Capture(id)});
tuple_enum!(CallTarget{0=>Register(reg),1=>Function(id),2=>Native(id)});
tuple_enum!(Arguments{0=>Window(window),1=>Packed(reg)});
impl Wire for Effects {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.0.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let x = u32::get(r)?;
        if x & !Effects::ALL.0 != 0 {
            return Err(r.error(ErrorKind::Encoding, "unknown effect"));
        }
        Ok(Self(x))
    }
}
record!(Parameter {
    name,
    ty,
    mutable,
    default
});
tuple_enum!(ParameterDefault{0=>Constant(id),1=>Function(id)});
record!(Signature {
    parameters,
    rest,
    return_type,
    effects,
    property_bag_tail
});
record!(PropertyBagTail {
    target_parameter,
    capability,
    require_writable,
    require_animatable
});
record!(NativeDef {
    symbol,
    signature,
    abi_version,
    capabilities
});
record!(GlobalDef {
    name,
    ty,
    mutable,
    linked_function
});
record!(Import {
    name,
    module,
    export,
    content_version
});
record!(Export { name, global });
record!(BindingDef {
    name,
    ty,
    mutable,
    storage,
    initialization
});
record!(NamedArg { name, offset });
record!(CallDesc {
    target,
    arguments,
    names,
    contract
});
record!(ClassField {
    name,
    ty,
    mutable,
    is_static,
    initializer,
    captures
});
record!(ClassMethod {
    name,
    function,
    is_static,
    captures
});
record!(ClassDesc {
    nominal,
    base,
    fields,
    methods
});
record!(PatternField {
    name,
    node,
    default,
    captures
});
record!(PatternDesc { nodes, root });
record!(PcRange { start, end });
record!(CatchRegion { range, exception });
record!(ExceptionRegion {
    protected,
    catch,
    finally
});
impl Wire for TextPoolDescriptor {
    const MIN: usize = 40;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.byte_len.put(w)?;
        w.write(&self.digest)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            byte_len: u64::get(r)?,
            digest: r.take(32)?.try_into().expect("text pool digest field"),
        })
    }
}
// The pinned miniz compressor's Huffman, output, dictionary and hash buffers
// together fit 256 KiB beyond its inline state. wasm32 boxes one extra 64 KiB
// code buffer. The allocator regression below checks this workspace bound.
pub(in crate::bytecode) const SOURCE_COMPRESSION_SCRATCH: usize = size_of::<
    miniz_oxide::deflate::core::CompressorOxide,
>() + 256 * 1024
    + if cfg!(target_arch = "wasm32") {
        64 * 1024
    } else {
        0
    };

pub(super) fn compressed_source(
    source: &[u8],
    mut write: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<usize, Error> {
    use miniz_oxide::deflate::core::{
        CompressorOxide, TDEFLFlush, TDEFLStatus, compress_to_output,
        create_comp_flags_from_zip_params,
    };
    let mut compressor = CompressorOxide::new(create_comp_flags_from_zip_params(1, 15, 0));
    let mut error = None;
    let mut len = 0usize;
    let (status, consumed) = compress_to_output(
        &mut compressor,
        source,
        TDEFLFlush::Finish,
        |part| match write(part) {
            Ok(()) => {
                len += part.len();
                true
            }
            Err(e) => {
                error = Some(e);
                false
            }
        },
    );
    if let Some(error) = error {
        return Err(error);
    }
    if status != TDEFLStatus::Done || consumed != source.len() {
        return Err(Error::new(ErrorKind::Encoding, "source compression failed"));
    }
    Ok(len)
}

impl Wire for SourceFile {
    const MIN: usize = 12;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.path.put(w)?;
        u32::try_from(self.text.len())
            .map_err(|_| Error::new(ErrorKind::Limit, "source length"))?
            .put(w)?;
        w.scratch(SOURCE_COMPRESSION_SCRATCH)?;
        if w.bytes.is_some() && w.digest.is_none() {
            // The output buffer can fill the length after its single pass.
            // A digest or measurement still consumes the canonical prefix.
            let prefix = w.len;
            0u32.put(w)?;
            let len = compressed_source(self.text.as_bytes(), |part| w.write(part))?;
            let len = u32::try_from(len)
                .map_err(|_| Error::new(ErrorKind::Limit, "packed source length"))?;
            w.bytes.as_mut().unwrap()[prefix..prefix + 4].copy_from_slice(&len.to_le_bytes());
            return Ok(());
        }
        let len = compressed_source(self.text.as_bytes(), |_| Ok(()))?;
        u32::try_from(len)
            .map_err(|_| Error::new(ErrorKind::Limit, "packed source length"))?
            .put(w)?;
        if w.bytes.is_none() && w.digest.is_none() {
            w.len = w
                .len
                .checked_add(len)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "encoded source size"))?;
            if w.len > w.max || w.len as u64 > w.limits.max_work {
                return Err(Error::new(ErrorKind::Limit, "encoded byte/work limit"));
            }
        } else {
            compressed_source(self.text.as_bytes(), |part| w.write(part))?;
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        use miniz_oxide::inflate::{
            TINFLStatus,
            core::{DecompressorOxide, decompress, inflate_flags},
        };
        let path = SymbolId::get(r)?;
        let len = u32::get(r)? as usize;
        let packed = u32::get(r)? as usize;
        r.charge(1, len)?;
        let materialize = r.materialize;
        let allocation = r
            .allocation
            .checked_add(size_of::<DecompressorOxide>())
            .ok_or_else(|| r.error(ErrorKind::Limit, "source inflation capacity"))?;
        if allocation > r.limits.max_allocation_bytes {
            return Err(r.error(ErrorKind::Limit, "source inflation capacity"));
        }
        if materialize {
            r.work = r
                .work
                .checked_add(len as u64 * 2)
                .ok_or_else(|| r.error(ErrorKind::Limit, "source inflation work"))?;
            if r.work > r.limits.max_work {
                return Err(r.error(ErrorKind::Limit, "source inflation work"));
            }
        }
        let bytes = r.take(packed)?;
        let text = if materialize {
            let mut output = Vec::new();
            output
                .try_reserve_exact(len)
                .map_err(|_| Error::new(ErrorKind::Allocation, "source output"))?;
            output.resize(len, 0);
            let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
                | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
            let (status, consumed, written) =
                decompress(&mut DecompressorOxide::new(), bytes, &mut output, 0, flags);
            if status != TINFLStatus::Done || consumed != packed || written != len {
                return Err(Error::new(ErrorKind::Encoding, "invalid compressed source"));
            }
            let text = String::from_utf8(output)
                .map_err(|_| Error::new(ErrorKind::Encoding, "source is not UTF-8"))?;
            r.charge(0, text.capacity() - len)?;
            text
        } else {
            String::new()
        };
        Ok(Self { path, text })
    }
}
record!(SourceSpan { file, start, end });

impl Wire for Const {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Null => 0u8.put(w)?,
            Self::Bool(x) => {
                1u8.put(w)?;
                x.put(w)?
            }
            Self::Int(x) => {
                2u8.put(w)?;
                x.put(w)?
            }
            Self::FloatBits(x) => {
                3u8.put(w)?;
                x.put(w)?
            }
            Self::Utf8(x) => {
                4u8.put(w)?;
                x.put(w)?
            }
            Self::Bytes(x) => {
                5u8.put(w)?;
                x.put(w)?
            }
            Self::Tuple(x) => {
                6u8.put(w)?;
                x.put(w)?
            }
            Self::Text { offset, len } => {
                7u8.put(w)?;
                offset.put(w)?;
                len.put(w)?;
            }
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Null),
            1 => Ok(Self::Bool(Wire::get(r)?)),
            2 => Ok(Self::Int(Wire::get(r)?)),
            3 => Ok(Self::FloatBits(Wire::get(r)?)),
            4 => Ok(Self::Utf8(Wire::get(r)?)),
            5 => Ok(Self::Bytes(Wire::get(r)?)),
            6 => Ok(Self::Tuple(Wire::get(r)?)),
            7 => Ok(Self::Text {
                offset: Wire::get(r)?,
                len: Wire::get(r)?,
            }),
            _ => Err(r.error(ErrorKind::Encoding, "constant tag")),
        }
    }
}
impl Wire for TypeDef {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Builtin(x) => {
                0u8.put(w)?;
                x.put(w)?
            }
            Self::Tuple(x) => {
                1u8.put(w)?;
                x.put(w)?
            }
            Self::Union(x) => {
                2u8.put(w)?;
                x.put(w)?
            }
            Self::Function {
                parameters,
                return_type,
            } => {
                3u8.put(w)?;
                parameters.put(w)?;
                return_type.put(w)?
            }
            Self::Named {
                module,
                name,
                version,
            } => {
                4u8.put(w)?;
                module.put(w)?;
                name.put(w)?;
                version.put(w)?
            }
            Self::StringLiteral(text) => {
                5u8.put(w)?;
                text.put(w)?;
            }
            Self::Record(fields) => {
                6u8.put(w)?;
                fields.put(w)?;
            }
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Builtin(Wire::get(r)?)),
            1 => Ok(Self::Tuple(Wire::get(r)?)),
            2 => Ok(Self::Union(Wire::get(r)?)),
            3 => Ok(Self::Function {
                parameters: Wire::get(r)?,
                return_type: Wire::get(r)?,
            }),
            4 => Ok(Self::Named {
                module: Wire::get(r)?,
                name: Wire::get(r)?,
                version: Wire::get(r)?,
            }),
            5 => Ok(Self::StringLiteral(Wire::get(r)?)),
            6 => Ok(Self::Record(Wire::get(r)?)),
            _ => Err(r.error(ErrorKind::Encoding, "type tag")),
        }
    }
}
impl Wire for RecordFieldType {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.name.put(w)?;
        self.ty.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            name: Wire::get(r)?,
            ty: Wire::get(r)?,
        })
    }
}
impl Wire for RegWindow {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.start.0.put(w)?;
        self.len.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let start = u32::get(r)?;
        let len = u32::get(r)?;
        if start.checked_add(len).is_none_or(|end| end > r.registers) {
            return Err(r.error(ErrorKind::Window, "register window"));
        }
        Ok(Self {
            start: Reg(start),
            len,
        })
    }
}
impl Wire for PatternNode {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Wildcard => 0u8.put(w)?,
            Self::Bind { name, output } => {
                1u8.put(w)?;
                name.put(w)?;
                output.put(w)?
            }
            Self::Literal(x) => {
                2u8.put(w)?;
                x.put(w)?
            }
            Self::Tuple { items, rest } => {
                3u8.put(w)?;
                items.put(w)?;
                rest.put(w)?
            }
            Self::Array { items, rest } => {
                4u8.put(w)?;
                items.put(w)?;
                rest.put(w)?
            }
            Self::Object { fields, rest } => {
                5u8.put(w)?;
                fields.put(w)?;
                rest.put(w)?
            }
            Self::Is { node, ty } => {
                6u8.put(w)?;
                node.put(w)?;
                ty.put(w)?
            }
            Self::Or { left, right } => {
                7u8.put(w)?;
                left.put(w)?;
                right.put(w)?
            }
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Wildcard),
            1 => Ok(Self::Bind {
                name: Wire::get(r)?,
                output: Wire::get(r)?,
            }),
            2 => Ok(Self::Literal(Wire::get(r)?)),
            3 => Ok(Self::Tuple {
                items: Wire::get(r)?,
                rest: Wire::get(r)?,
            }),
            4 => Ok(Self::Array {
                items: Wire::get(r)?,
                rest: Wire::get(r)?,
            }),
            5 => Ok(Self::Object {
                fields: Wire::get(r)?,
                rest: Wire::get(r)?,
            }),
            6 => Ok(Self::Is {
                node: Wire::get(r)?,
                ty: Wire::get(r)?,
            }),
            7 => Ok(Self::Or {
                left: Wire::get(r)?,
                right: Wire::get(r)?,
            }),
            _ => Err(r.error(ErrorKind::Encoding, "pattern tag")),
        }
    }
}
tuple_enum!(TableSource { 0 => Pair(pair), 1 => Spread(reg) });
impl Wire for Descriptor {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            Self::Global { module, global } => {
                0u8.put(w)?;
                module.put(w)?;
                global.put(w)?
            }
            Self::Closure { function, captures } => {
                1u8.put(w)?;
                function.put(w)?;
                captures.put(w)?
            }
            Self::Call(x) => {
                2u8.put(w)?;
                x.put(w)?
            }
            Self::Sequence(x) => {
                3u8.put(w)?;
                x.put(w)?
            }
            Self::Table(x) => {
                4u8.put(w)?;
                x.put(w)?
            }
            Self::TableSources(x) => {
                10u8.put(w)?;
                x.put(w)?
            }
            Self::Class(x) => {
                5u8.put(w)?;
                x.put(w)?
            }
            Self::Super { base, member } => {
                6u8.put(w)?;
                base.put(w)?;
                member.put(w)?
            }
            Self::Property { key, writable } => {
                7u8.put(w)?;
                key.put(w)?;
                writable.put(w)?
            }
            Self::Pattern(x) => {
                8u8.put(w)?;
                x.put(w)?
            }
            Self::Spawn { call, owner } => {
                9u8.put(w)?;
                call.put(w)?;
                owner.put(w)?
            }
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => {
                let module = ModuleId::get(r)?;
                let global = GlobalId(u32::get(r)?);
                r.index(global.0, r.globals[module.index()])?;
                Ok(Self::Global { module, global })
            }
            1 => Ok(Self::Closure {
                function: Wire::get(r)?,
                captures: Wire::get(r)?,
            }),
            2 => Ok(Self::Call(Wire::get(r)?)),
            3 => Ok(Self::Sequence(Wire::get(r)?)),
            4 => Ok(Self::Table(Wire::get(r)?)),
            10 => Ok(Self::TableSources(Wire::get(r)?)),
            5 => Ok(Self::Class(Wire::get(r)?)),
            6 => Ok(Self::Super {
                base: Wire::get(r)?,
                member: Wire::get(r)?,
            }),
            7 => Ok(Self::Property {
                key: Wire::get(r)?,
                writable: Wire::get(r)?,
            }),
            8 => Ok(Self::Pattern(Wire::get(r)?)),
            9 => Ok(Self::Spawn {
                call: Wire::get(r)?,
                owner: Wire::get(r)?,
            }),
            _ => Err(r.error(ErrorKind::Encoding, "descriptor tag")),
        }
    }
}
impl Wire for ModuleDef {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        (self.globals.len() as u32).put(w)?;
        self.path.put(w)?;
        self.imports.put(w)?;
        self.exports.put(w)?;
        self.globals.put(w)?;
        self.initializer.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let globals = u32::get(r)?;
        if globals > r.limits.max_records {
            return Err(r.error(ErrorKind::Limit, "module globals"));
        }
        r.global_bound = globals;
        // This small, bounded count directory is the only scan-pass allocation.
        let module = r.globals.len();
        r.globals.push(globals);
        let path = Wire::get(r)?;
        let (imports, import_count) = counted_vec(r)?;
        let bindings = globals
            .checked_add(import_count as u32)
            .ok_or_else(|| r.error(ErrorKind::Limit, "module binding count"))?;
        r.globals[module] = bindings;
        r.global_bound = bindings;
        let out = Self {
            path,
            imports,
            exports: Wire::get(r)?,
            globals: r.expected(globals)?,
            initializer: Wire::get(r)?,
        };
        if r.materialize && out.globals.len() != globals as usize {
            return Err(r.error(ErrorKind::Encoding, "global count mismatch"));
        }
        if r.materialize {
            for export in &out.exports {
                r.index(export.global.0, bindings)?;
            }
        }
        Ok(out)
    }
}

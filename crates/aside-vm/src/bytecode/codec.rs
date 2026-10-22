//! Explicit little-endian wire codec. The first pass materializes no payloads.
use super::*;
use std::mem::size_of;

const MAGIC: &[u8; 8] = b"ASIDBC01";
const HEADER_BYTES: usize = 68;

trait Wire: Sized {
    const MIN: usize = 1;
    fn put(&self, w: &mut Writer) -> Result<(), Error>;
    fn get(r: &mut Reader<'_>) -> Result<Self, Error>;
}
struct Writer {
    bytes: Option<Vec<u8>>,
    digest: Option<blake3::Hasher>,
    len: usize,
    max: usize,
    capacity: usize,
    records: u64,
    limits: Limits,
}
impl Writer {
    fn capacity(&mut self, records: usize, bytes: usize) -> Result<(), Error> {
        self.capacity = self
            .capacity
            .checked_add(bytes)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "capacity overflow"))?;
        self.records = self
            .records
            .checked_add(records as u64)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "record overflow"))?;
        if self.capacity > self.limits.max_allocation_bytes {
            return Err(Error::new(
                ErrorKind::Limit,
                format!(
                    "allocation quota: requested {} bytes, limit {} bytes",
                    self.capacity, self.limits.max_allocation_bytes
                ),
            ));
        }
        if self.records > self.limits.max_records as u64 {
            return Err(Error::new(
                ErrorKind::Limit,
                format!(
                    "record quota: requested {} records, limit {} records",
                    self.records, self.limits.max_records
                ),
            ));
        }
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.len = self
            .len
            .checked_add(bytes.len())
            .ok_or_else(|| Error::new(ErrorKind::Limit, "encoded length overflow"))?;
        if self.len > self.max || self.len as u64 > self.limits.max_work {
            return Err(Error::new(ErrorKind::Limit, "encoded byte/work limit"));
        }
        if let Some(digest) = &mut self.digest {
            digest.update(bytes);
        }
        if let Some(out) = &mut self.bytes {
            out.extend_from_slice(bytes);
        }
        Ok(())
    }
}
struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
    limits: Limits,
    materialize: bool,
    records: u32,
    allocation: usize,
    work: u64,
    counts: [u32; 9],
    globals: Vec<u32>,
    registers: u32,
    bindings: u32,
    captures: u32,
    descriptors: u32,
    code: u32,
    regions: u32,
    global_bound: u32,
}
impl Reader<'_> {
    fn error(&self, kind: ErrorKind, detail: &str) -> Error {
        let mut e = Error::new(kind, detail);
        e.offset = Some(self.pos);
        e
    }
    fn take(&mut self, len: usize) -> Result<&[u8], Error> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| self.error(ErrorKind::Limit, "length overflow"))?;
        let bytes = self
            .input
            .get(self.pos..end)
            .ok_or_else(|| self.error(ErrorKind::Truncated, "incomplete field"))?;
        self.work = self
            .work
            .checked_add(len as u64)
            .ok_or_else(|| self.error(ErrorKind::Limit, "work overflow"))?;
        if self.work > self.limits.max_work {
            return Err(self.error(ErrorKind::Limit, "decoder work limit"));
        }
        self.pos = end;
        Ok(bytes)
    }
    fn charge(&mut self, records: u32, bytes: usize) -> Result<(), Error> {
        self.records = self
            .records
            .checked_add(records)
            .ok_or_else(|| self.error(ErrorKind::Limit, "record count overflow"))?;
        self.allocation = self
            .allocation
            .checked_add(bytes)
            .ok_or_else(|| self.error(ErrorKind::Limit, "allocation overflow"))?;
        if self.records > self.limits.max_records {
            return Err(self.error(
                ErrorKind::Limit,
                &format!(
                    "record quota: requested {} records, limit {} records",
                    self.records, self.limits.max_records
                ),
            ));
        }
        if self.allocation > self.limits.max_allocation_bytes {
            return Err(self.error(
                ErrorKind::Limit,
                &format!(
                    "allocation quota: requested {} bytes, limit {} bytes",
                    self.allocation, self.limits.max_allocation_bytes
                ),
            ));
        }
        Ok(())
    }
    fn count<T: Wire>(&mut self) -> Result<usize, Error> {
        let count = u32::get(self)?;
        let len = count as usize;
        let minimum = len
            .checked_mul(T::MIN)
            .ok_or_else(|| self.error(ErrorKind::Limit, "count multiplication overflow"))?;
        if minimum > self.input.len() - self.pos {
            return Err(self.error(ErrorKind::Truncated, "count exceeds remaining fields"));
        }
        let memory = len
            .checked_mul(size_of::<T>())
            .ok_or_else(|| self.error(ErrorKind::Limit, "capacity multiplication overflow"))?;
        self.charge(count, memory)?;
        Ok(len)
    }
    fn index(&self, index: u32, bound: u32) -> Result<(), Error> {
        if index >= bound {
            Err(self.error(ErrorKind::Index, "index outside declared table"))
        } else {
            Ok(())
        }
    }
    fn expected<T: Wire>(&mut self, count: u32) -> Result<Vec<T>, Error> {
        let end = self
            .pos
            .checked_add(4)
            .ok_or_else(|| self.error(ErrorKind::Limit, "count offset overflow"))?;
        let bytes = self
            .input
            .get(self.pos..end)
            .ok_or_else(|| self.error(ErrorKind::Truncated, "table count"))?;
        if u32::from_le_bytes(bytes.try_into().expect("count field")) != count {
            return Err(self.error(ErrorKind::Encoding, "header/body count mismatch"));
        }
        Vec::get(self)
    }
}

macro_rules! integer {
    ($($t:ty),+) => {$ (
        impl Wire for $t {
            const MIN: usize = size_of::<Self>();
            fn put(&self, w: &mut Writer) -> Result<(), Error> { w.write(&self.to_le_bytes()) }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                let b = r.take(size_of::<Self>())?;
                Ok(Self::from_le_bytes(b.try_into().expect("fixed field")))
            }
        }
    )+};
}
integer!(u8, u16, u32, u64, i64);
impl Wire for bool {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        (*self as u8).put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(r.error(ErrorKind::Encoding, "invalid bool")),
        }
    }
}
impl<T: Wire> Wire for Option<T> {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            None => 0u8.put(w),
            Some(x) => {
                1u8.put(w)?;
                x.put(w)
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(r)?)),
            _ => Err(r.error(ErrorKind::Encoding, "invalid option")),
        }
    }
}
impl<T: Wire> Wire for Vec<T> {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        w.capacity(
            self.len(),
            self.capacity()
                .checked_mul(size_of::<T>())
                .ok_or_else(|| Error::new(ErrorKind::Limit, "vector capacity"))?,
        )?;
        u32::try_from(self.len())
            .map_err(|_| Error::new(ErrorKind::Limit, "vector length"))?
            .put(w)?;
        for x in self {
            x.put(w)?;
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        counted_vec(r).map(|(values, _)| values)
    }
}
fn counted_vec<T: Wire>(r: &mut Reader<'_>) -> Result<(Vec<T>, usize), Error> {
    let count = r.count::<T>()?;
    let mut out = Vec::new();
    if r.materialize {
        out.try_reserve_exact(count)
            .map_err(|_| r.error(ErrorKind::Allocation, "vector reservation"))?;
        r.charge(
            0,
            (out.capacity() - count)
                .checked_mul(size_of::<T>())
                .ok_or_else(|| r.error(ErrorKind::Limit, "capacity overflow"))?,
        )?;
    }
    for _ in 0..count {
        let x = T::get(r)?;
        if r.materialize {
            out.push(x);
        }
    }
    Ok((out, count))
}
impl Wire for String {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        w.capacity(1, self.capacity())?;
        u32::try_from(self.len())
            .map_err(|_| Error::new(ErrorKind::Limit, "string length"))?
            .put(w)?;
        w.write(self.as_bytes())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let count = u32::get(r)?;
        r.charge(1, count as usize)?;
        let materialize = r.materialize;
        let mut out = String::new();
        let text = std::str::from_utf8(r.take(count as usize)?)
            .map_err(|_| Error::new(ErrorKind::Encoding, "invalid UTF-8"))?;
        if materialize {
            out.try_reserve_exact(text.len())
                .map_err(|_| Error::new(ErrorKind::Allocation, "string reservation"))?;
            out.push_str(text);
            r.charge(0, out.capacity() - count as usize)?;
        }
        Ok(out)
    }
}
impl Wire for [Reg; 2] {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self[0].put(w)?;
        self[1].put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok([Reg::get(r)?, Reg::get(r)?])
    }
}
macro_rules! index {
    ($($t:ident => $bound:expr),+ $(,)?) => {$ (
        impl Wire for $t {
            const MIN: usize = 4;
            fn put(&self,w:&mut Writer)->Result<(),Error>{self.0.put(w)}
            fn get(r:&mut Reader<'_>)->Result<Self,Error>{ let x=u32::get(r)?;let bound=$bound; r.index(x,bound(r))?;Ok(Self(x)) }
        }
    )+};
}
index!(SymbolId => |r:&Reader<'_>| r.counts[0], TypeId => |r:&Reader<'_>| r.counts[1],
    ConstId => |r:&Reader<'_>| r.counts[2], SignatureId => |r:&Reader<'_>| r.counts[3],
    NativeId => |r:&Reader<'_>| r.counts[4], ModuleId => |r:&Reader<'_>| r.counts[5],
    FunctionId => |r:&Reader<'_>| r.counts[6], SourceId => |r:&Reader<'_>| r.counts[8],
    Reg => |r:&Reader<'_>| r.registers, BindingId => |r:&Reader<'_>| r.bindings,
    CaptureId => |r:&Reader<'_>| r.captures, DescriptorId => |r:&Reader<'_>| r.descriptors,
    RegionId => |r:&Reader<'_>| r.regions);
impl Wire for GlobalId {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.0.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let x = u32::get(r)?;
        r.index(x, r.global_bound)?;
        Ok(Self(x))
    }
}
impl Wire for Pc {
    const MIN: usize = 4;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.0.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let x = u32::get(r)?;
        if x > r.code {
            return Err(r.error(ErrorKind::Index, "PC past code"));
        }
        Ok(Self(x))
    }
}
macro_rules! record {
    ($t:ident {$($field:ident),+ $(,)?}) => {
        impl Wire for $t {
            fn put(&self,w:&mut Writer)->Result<(),Error>{ $(self.$field.put(w)?;)+ Ok(()) }
            fn get(r:&mut Reader<'_>)->Result<Self,Error>{ Ok(Self {$($field: Wire::get(r)?,)+}) }
        }
    };
}
macro_rules! unit_enum {
    ($t:ident {$($tag:literal => $v:ident),+ $(,)?}) => {
        impl Wire for $t {
            fn put(&self,w:&mut Writer)->Result<(),Error>{let tag:u8=match self {$(Self::$v=>$tag,)+};tag.put(w)}
            fn get(r:&mut Reader<'_>)->Result<Self,Error>{match u8::get(r)? { $($tag=>Ok(Self::$v),)+ _=>Err(r.error(ErrorKind::Encoding,concat!("unknown ",stringify!($t)," tag"))) } }
        }
    };
}
macro_rules! tuple_enum {
    ($t:ident {$($tag:literal => $v:ident($($x:ident),+)),+ $(,)?}) => {
        impl Wire for $t {
            fn put(&self,w:&mut Writer)->Result<(),Error>{match self {$ (Self::$v($($x),+) => {($tag as u8).put(w)?;$($x.put(w)?;)+},)+}Ok(())}
            fn get(r:&mut Reader<'_>)->Result<Self,Error>{match u8::get(r)? {$($tag=>Ok(Self::$v($(tuple_enum!(@get r $x)),+)),)+ _=>Err(r.error(ErrorKind::Encoding,concat!("unknown ",stringify!($t)," tag")))}}
        }
    };
    (@get $r:ident $x:ident) => {Wire::get($r)?};
}
unit_enum!(BuiltinType {0=>Any,1=>Null,2=>Bool,3=>Int,4=>Float,5=>String,6=>Bytes,7=>Array,8=>Table,9=>Tuple,10=>Class,11=>Instance,12=>Closure,13=>Generator,14=>Thread,15=>Future,16=>WeakRef,17=>PropertyRef});
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
    names
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
record!(SourceFile { path, text });
record!(SourceSpan { file, start, end });
record!(SourceMapEntry { pc, span });

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
            Self::Function(x) => {
                3u8.put(w)?;
                x.put(w)?
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
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(Self::Builtin(Wire::get(r)?)),
            1 => Ok(Self::Tuple(Wire::get(r)?)),
            2 => Ok(Self::Union(Wire::get(r)?)),
            3 => Ok(Self::Function(Wire::get(r)?)),
            4 => Ok(Self::Named {
                module: Wire::get(r)?,
                name: Wire::get(r)?,
                version: Wire::get(r)?,
            }),
            _ => Err(r.error(ErrorKind::Encoding, "type tag")),
        }
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
impl Wire for FunctionDef {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        for len in [
            self.captures.len(),
            self.bindings.len(),
            self.descriptors.len(),
            self.code.len(),
            self.regions.len(),
            self.source_map.len(),
        ] {
            (len as u32).put(w)?;
        }
        self.register_count.put(w)?;
        self.module.put(w)?;
        self.name.put(w)?;
        self.kind.put(w)?;
        self.signature.put(w)?;
        self.context.put(w)?;
        self.captures.put(w)?;
        self.bindings.put(w)?;
        self.descriptors.put(w)?;
        self.code.put(w)?;
        self.regions.put(w)?;
        self.source_map.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let lens: [u32; 6] = [
            u32::get(r)?,
            u32::get(r)?,
            u32::get(r)?,
            u32::get(r)?,
            u32::get(r)?,
            u32::get(r)?,
        ];
        if lens.iter().any(|n| *n > r.limits.max_records) {
            return Err(r.error(ErrorKind::Limit, "function table counts"));
        }
        [r.captures, r.bindings, r.descriptors, r.code, r.regions] =
            [lens[0], lens[1], lens[2], lens[3], lens[4]];
        r.registers = u32::get(r)?;
        if r.registers > r.limits.max_registers {
            return Err(r.error(ErrorKind::Limit, "register limit"));
        }
        let out = Self {
            register_count: r.registers,
            module: Wire::get(r)?,
            name: Wire::get(r)?,
            kind: Wire::get(r)?,
            signature: Wire::get(r)?,
            context: Wire::get(r)?,
            captures: r.expected(lens[0])?,
            bindings: r.expected(lens[1])?,
            descriptors: r.expected(lens[2])?,
            code: r.expected(lens[3])?,
            regions: r.expected(lens[4])?,
            source_map: r.expected(lens[5])?,
        };
        if r.materialize
            && [
                out.captures.len(),
                out.bindings.len(),
                out.descriptors.len(),
                out.code.len(),
                out.regions.len(),
                out.source_map.len(),
            ] != lens.map(|x| x as usize)
        {
            return Err(r.error(ErrorKind::Encoding, "function count mismatch"));
        }
        Ok(out)
    }
}
impl Wire for Instruction {
    const MIN: usize = 16;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        w.write(&self.to_le_bytes())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let instruction = Self::from_le_bytes(r.take(16)?.try_into().expect("instruction field"))?;
        let spec = instruction_spec(instruction.opcode).expect("decoded opcode");
        for (kind, value) in spec.operands.iter().zip(instruction.operands) {
            use OperandKind::*;
            match kind {
                Unused if value != 0 => {
                    return Err(r.error(ErrorKind::Encoding, "nonzero unused operand"));
                }
                ReadReg | WriteReg | ReadWriteReg => r.index(value, r.registers)?,
                Const => r.index(value, r.counts[2])?,
                Symbol => r.index(value, r.counts[0])?,
                Type => r.index(value, r.counts[1])?,
                Binding | BindingRead | BindingWrite | BindingInit => r.index(value, r.bindings)?,
                Capture => r.index(value, r.captures)?,
                Pc => r.index(value, r.code)?,
                Region => r.index(value, r.regions)?,
                Descriptor(_) => r.index(value, r.descriptors)?,
                _ => {}
            }
        }
        Ok(instruction)
    }
}
impl Wire for BytecodePackage {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.text_pool.put(w)?;
        self.symbols.put(w)?;
        self.types.put(w)?;
        self.constants.put(w)?;
        self.signatures.put(w)?;
        self.natives.put(w)?;
        self.modules.put(w)?;
        self.functions.put(w)?;
        self.capabilities.put(w)?;
        self.sources.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            text_pool: Wire::get(r)?,
            symbols: r.expected(r.counts[0])?,
            types: r.expected(r.counts[1])?,
            constants: r.expected(r.counts[2])?,
            signatures: r.expected(r.counts[3])?,
            natives: r.expected(r.counts[4])?,
            modules: r.expected(r.counts[5])?,
            functions: r.expected(r.counts[6])?,
            capabilities: r.expected(r.counts[7])?,
            sources: r.expected(r.counts[8])?,
            ..Self::default()
        })
    }
}

fn counts(p: &BytecodePackage) -> Result<[u32; 9], Error> {
    let lens = [
        p.symbols.len(),
        p.types.len(),
        p.constants.len(),
        p.signatures.len(),
        p.natives.len(),
        p.modules.len(),
        p.functions.len(),
        p.capabilities.len(),
        p.sources.len(),
    ];
    let mut out = [0; 9];
    for (dst, len) in out.iter_mut().zip(lens) {
        *dst = u32::try_from(len).map_err(|_| Error::new(ErrorKind::Limit, "section count"))?
    }
    Ok(out)
}
/// Encodes validated fields; does not serialize Rust layout or runtime values.
pub fn encode(package: &BytecodePackage, limits: Limits) -> Result<Vec<u8>, Error> {
    let stats = super::verify::validate(package, limits)?;
    // Validation's derived cache has been dropped; only the package remains
    // resident while encoding allocates its output buffer.
    encode_fields(
        package,
        limits,
        stats.owned_capacity_bytes - stats.derived_capacity_bytes,
    )
}
fn encode_fields(
    package: &BytecodePackage,
    limits: Limits,
    owned: usize,
) -> Result<Vec<u8>, Error> {
    let mut measure = Writer {
        digest: None,
        bytes: None,
        len: HEADER_BYTES,
        max: limits.max_bytes,
        capacity: 0,
        records: 0,
        limits,
    };
    package.put(&mut measure)?;
    let len =
        u32::try_from(measure.len).map_err(|_| Error::new(ErrorKind::Limit, "package size"))?;
    if measure.len > limits.max_allocation_bytes {
        return Err(Error::new(ErrorKind::Limit, "output allocation limit"));
    }
    if owned
        .checked_add(measure.len)
        .is_none_or(|n| n > limits.max_allocation_bytes)
    {
        return Err(Error::new(
            ErrorKind::Limit,
            "package/output allocation limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(measure.len)
        .map_err(|_| Error::new(ErrorKind::Allocation, "encoded output"))?;
    if owned
        .checked_add(bytes.capacity())
        .is_none_or(|n| n > limits.max_allocation_bytes)
    {
        return Err(Error::new(
            ErrorKind::Limit,
            "package/output capacity limit",
        ));
    }
    let mut w = Writer {
        digest: None,
        bytes: Some(bytes),
        len: 0,
        max: limits.max_bytes,
        capacity: 0,
        records: 0,
        limits,
    };
    w.write(MAGIC)?;
    package.format_version.put(&mut w)?;
    package.language_version.put(&mut w)?;
    package.host_abi_version.put(&mut w)?;
    package.content_version.put(&mut w)?;
    len.put(&mut w)?;
    for n in counts(package)? {
        n.put(&mut w)?;
    }
    package.put(&mut w)?;
    Ok(w.bytes.expect("output writer"))
}
/// Complete allocation-bounded shape/index scan, then fallible materialization.
/// Semantic verification and runtime capability checks are explicit next steps.
pub fn decode(input: &[u8], limits: Limits) -> Result<BytecodePackage, Error> {
    if input.len() > limits.max_bytes {
        return Err(Error::new(ErrorKind::Limit, "input bytes"));
    }
    let mut r = Reader {
        input,
        pos: 0,
        limits,
        materialize: false,
        records: 0,
        allocation: size_of::<BytecodePackage>(),
        work: 0,
        counts: [0; 9],
        globals: Vec::new(),
        registers: 0,
        bindings: 0,
        captures: 0,
        descriptors: 0,
        code: 0,
        regions: 0,
        global_bound: 0,
    };
    if r.take(8)? != MAGIC {
        return Err(r.error(ErrorKind::Encoding, "magic"));
    }
    let format = u32::get(&mut r)?;
    let language = u32::get(&mut r)?;
    let abi = u32::get(&mut r)?;
    let content = u64::get(&mut r)?;
    let len = u32::get(&mut r)?;
    if (format, language, abi) != (FORMAT_VERSION, LANGUAGE_VERSION, HOST_ABI_VERSION) {
        return Err(r.error(ErrorKind::Version, "unsupported package/language/host ABI"));
    }
    if len as usize != input.len() {
        return Err(r.error(ErrorKind::Encoding, "declared total byte length"));
    }
    for i in 0..9 {
        r.counts[i] = u32::get(&mut r)?;
    }
    let records = r
        .counts
        .iter()
        .try_fold(0u32, |a, b| a.checked_add(*b))
        .ok_or_else(|| r.error(ErrorKind::Limit, "section count overflow"))?;
    if records > limits.max_records {
        return Err(r.error(ErrorKind::Limit, "section counts"));
    }
    if records as usize > input.len() - HEADER_BYTES {
        return Err(r.error(ErrorKind::Truncated, "section counts exceed input"));
    }
    let directory = (r.counts[5] as usize)
        .checked_mul(size_of::<u32>())
        .ok_or_else(|| r.error(ErrorKind::Limit, "module directory size"))?;
    if directory > limits.max_allocation_bytes {
        return Err(r.error(ErrorKind::Limit, "module directory allocation"));
    }
    r.globals
        .try_reserve_exact(r.counts[5] as usize)
        .map_err(|_| r.error(ErrorKind::Allocation, "module count directory"))?;
    r.charge(0, r.globals.capacity() * size_of::<u32>())?;
    let allocation_base = r.allocation;
    let _scan = BytecodePackage::get(&mut r)?;
    if r.pos != input.len() {
        return Err(r.error(ErrorKind::Encoding, "trailing bytes"));
    }
    // The count directory is reused; payload capacity was bounded in the scan.
    let scanned_work = r.work;
    r.pos = HEADER_BYTES;
    r.materialize = true;
    r.records = 0;
    r.allocation = allocation_base;
    r.work = scanned_work;
    r.globals.clear();
    let mut package = BytecodePackage::get(&mut r)?;
    if counts(&package)? != r.counts {
        return Err(r.error(ErrorKind::Encoding, "section count mismatch"));
    }
    package.format_version = format;
    package.language_version = language;
    package.host_abi_version = abi;
    package.content_version = content;
    Ok(package)
}

pub(crate) fn measure(
    package: &BytecodePackage,
    limits: Limits,
) -> Result<(usize, usize, u64), Error> {
    let mut w = Writer {
        digest: None,
        bytes: None,
        len: HEADER_BYTES,
        max: limits.max_bytes,
        capacity: size_of::<BytecodePackage>(),
        records: 0,
        limits,
    };
    package.put(&mut w)?;
    Ok((w.len, w.capacity, w.records))
}
#[cfg(test)]
pub(super) fn encode_unverified(package: &BytecodePackage) -> Vec<u8> {
    encode_fields(package, Limits::default(), 0).expect("bounded malformed test fixture")
}

// Hash canonical fields without materializing another copy of the code package.
pub(crate) fn identity(package: &BytecodePackage, limits: Limits) -> Result<[u8; 32], Error> {
    let mut writer = Writer {
        bytes: None,
        digest: Some(blake3::Hasher::new()),
        len: 0,
        max: limits.max_bytes,
        capacity: 0,
        records: 0,
        limits,
    };
    package.format_version.put(&mut writer)?;
    package.language_version.put(&mut writer)?;
    package.host_abi_version.put(&mut writer)?;
    package.content_version.put(&mut writer)?;
    package.put(&mut writer)?;
    Ok(*writer.digest.unwrap().finalize().as_bytes())
}

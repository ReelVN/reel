//! Explicit little-endian wire codec. The first pass materializes no payloads.
use super::code_residency::{CodeBlock, CodeStore, CodeStoreBuilder, CodeWindow, PAGE_WORDS};
use super::*;
use std::mem::size_of;
use std::sync::Arc;

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
    retained: usize,
    #[cfg(test)]
    unverified_fixture: bool,
    records: u64,
    code_store: Option<Arc<CodeStore>>,
    function_word: u32,
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
    code_store: Option<Arc<CodeStore>>,
    function_word: u32,
}
impl<'a> Reader<'a> {
    fn new(input: &'a [u8], limits: Limits) -> Self {
        Self {
            input,
            pos: 0,
            limits,
            materialize: false,
            records: 0,
            allocation: 0,
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
            code_store: None,
            function_word: 0,
        }
    }

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
fn put_function(code: &FunctionDef<impl InstructionSequence>, w: &mut Writer) -> Result<(), Error> {
    for len in [
        code.captures.len(),
        code.bindings.len(),
        code.descriptors.len(),
        code.code.len(),
        code.regions.len(),
        code.source_map.len(),
    ] {
        (len as u32).put(w)?;
    }
    code.register_count.put(w)?;
    code.module.put(w)?;
    code.name.put(w)?;
    code.kind.put(w)?;
    code.signature.put(w)?;
    code.context.put(w)?;
    code.captures.put(w)?;
    code.bindings.put(w)?;
    code.descriptors.put(w)?;
    let words = if code.code.runtime_words().is_some() {
        code.code.end_pc()
    } else {
        code.code.iter().try_fold(0usize, |n, i| {
            n.checked_add(1 + CompactCode::shape(&i?)?.3)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "code words"))
        })?
    };
    w.capacity(0, code.code.capacity_bytes())?;
    let start = w.function_word;
    start.put(w)?;
    u32::try_from(words)
        .map_err(|_| Error::new(ErrorKind::Limit, "code words"))?
        .put(w)?;
    w.function_word = w
        .function_word
        .checked_add(words as u32)
        .ok_or_else(|| Error::new(ErrorKind::Limit, "code range"))?;
    if code.code.runtime_words().is_some() || (w.bytes.is_none() && w.digest.is_none()) {
        put_pc_metadata(code, w, |pc, _| Ok(pc))?;
    } else {
        let raw = code.code.editable().expect("editable code");
        let map_bytes = (raw.len() + 1)
            .checked_mul(4)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "PC map bytes"))?;
        let output = w.bytes.as_ref().map_or(0, Vec::capacity);
        if w.retained
            .checked_add(output)
            .and_then(|n| n.checked_add(map_bytes))
            .is_none_or(|n| n > w.limits.max_allocation_bytes)
        {
            return Err(Error::new(ErrorKind::Limit, "encoding PC map"));
        }
        let mut pcs = Vec::with_capacity(raw.len() + 1);
        let mut pc = 0;
        for i in raw {
            pcs.push(pc);
            pc += 1 + CompactCode::shape(i)?.3 as u32;
        }
        pcs.push(pc);
        put_pc_metadata(code, w, |target, writer| encode_pc(&pcs, target, writer))?;
    }
    Ok(())
}
fn encode_pc(pcs: &[u32], target: Pc, w: &Writer) -> Result<Pc, Error> {
    if let Some(&pc) = pcs.get(target.index()) {
        return Ok(Pc(pc));
    }
    #[cfg(test)]
    if w.unverified_fixture {
        return Ok(Pc(pcs
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(target.0)));
    }
    let _ = w;
    Err(Error::new(ErrorKind::Index, "encoding PC"))
}
fn put_pc_metadata(
    code: &FunctionDef<impl InstructionSequence>,
    w: &mut Writer,
    map: impl Fn(Pc, &Writer) -> Result<Pc, Error>,
) -> Result<(), Error> {
    w.capacity(
        code.regions.len(),
        code.regions.capacity() * size_of::<ExceptionRegion>(),
    )?;
    (code.regions.len() as u32).put(w)?;
    for region in &code.regions {
        let mut region = region.clone();
        super::compact::map_metadata_pcs(std::slice::from_mut(&mut region), &mut [], |pc| {
            map(pc, w)
        })?;
        region.put(w)?;
    }
    w.capacity(
        code.source_map.len(),
        code.source_map.capacity() * size_of::<SourceMapEntry>(),
    )?;
    (code.source_map.len() as u32).put(w)?;
    for entry in &code.source_map {
        SourceMapEntry {
            pc: map(entry.pc, w)?,
            span: entry.span,
        }
        .put(w)?;
    }
    Ok(())
}

impl Wire for RuntimeFunction {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        put_function(self, w)
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
            code: CompactCode::get(r)?,
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
impl Wire for CompactCode {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        w.capacity(self.end_pc(), self.capacity_bytes())?;
        (self.end_pc() as u32).put(w)?;
        for word in self.words() {
            word?.put(w)?;
        }
        Ok(())
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let start = u32::get(r)?;
        let words = u32::get(r)?;
        let count = r.code;
        let store = r
            .code_store
            .as_ref()
            .expect("execution store precedes functions")
            .clone();
        if start != r.function_word {
            return Err(r.error(ErrorKind::Encoding, "noncontiguous function code"));
        }
        r.function_word = start
            .checked_add(words)
            .ok_or_else(|| r.error(ErrorKind::Limit, "function code range"))?;
        let code = CompactCode::from_store(store.clone(), start, words, count)?;
        r.code = words;
        if !r.materialize {
            let mut window = CodeWindow::default();
            let actual = CompactCode::try_scan_words_checked(
                words as usize,
                |pc| store.word(start as usize + pc, &mut window),
                |i| check_instruction_indices(i, r),
            )?;
            if actual != count as usize {
                return Err(r.error(ErrorKind::Encoding, "instruction count mismatch"));
            }
        }
        Ok(code)
    }
}
fn check_instruction_indices(instruction: &Instruction, r: &Reader<'_>) -> Result<(), Error> {
    for (kind, value) in instruction_spec(instruction.opcode)
        .expect("checked opcode")
        .operands
        .iter()
        .zip(instruction.operands)
    {
        use OperandKind::*;
        match kind {
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
    Ok(())
}
fn prepare_code_store(
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
    owned: usize,
    #[cfg(test)] unverified: bool,
) -> Result<Arc<CodeStore>, Error> {
    if let Some(store) = package
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
        && package.functions.iter().all(|f| {
            f.code
                .runtime_store()
                .is_some_and(|other| Arc::ptr_eq(store, other))
        })
    {
        return Ok(store.clone());
    }
    let mut builder = CodeStoreBuilder::new();
    let mut words = 0usize;
    let mut temporary = 0usize;
    for f in &package.functions {
        let count = f.code.iter().try_fold(0usize, |n, i| {
            n.checked_add(1 + CompactCode::shape(&i?)?.3)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "code words"))
        })?;
        words = words
            .checked_add(count)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "code words"))?;
        temporary = temporary.max(
            count
                .checked_mul(4)
                .and_then(|n| n.checked_add((f.code.len() + 1) * 4))
                .ok_or_else(|| Error::new(ErrorKind::Limit, "code mapping capacity"))?,
        );
    }
    let packed_bound = words
        .checked_mul(4)
        .and_then(|n| n.checked_add(words / 250 + words.div_ceil(PAGE_WORDS) * 128))
        .ok_or_else(|| Error::new(ErrorKind::Limit, "code packing capacity"))?;
    let bound = CodeStore::planned_capacity_bytes(
        u32::try_from(words).map_err(|_| Error::new(ErrorKind::Limit, "code words"))?,
        packed_bound,
        words.div_ceil(PAGE_WORDS),
    )?;
    if owned
        .checked_add(bound)
        .and_then(|n| n.checked_add(builder.capacity_bytes()))
        .and_then(|n| n.checked_add(temporary))
        .is_none_or(|n| n > limits.max_allocation_bytes)
    {
        return Err(Error::new(ErrorKind::Limit, "code packing capacity"));
    }
    if words as u64 * 4 > limits.max_work {
        return Err(Error::new(ErrorKind::Limit, "code packing work"));
    }
    for f in &package.functions {
        if let Some(words) = f.code.runtime_words() {
            builder.append(words)?;
        } else {
            let raw = f.code.editable().expect("editable code");
            #[cfg(test)]
            if unverified {
                append_fixture_words(&mut builder, raw)?;
                continue;
            }
            let (code, _) = CompactCode::from_ordinal(raw)?;
            builder.append(code.words())?;
        }
    }
    builder.finish()
}
#[cfg(test)]
fn append_fixture_words(builder: &mut CodeStoreBuilder, raw: &[Instruction]) -> Result<(), Error> {
    let mut pcs = Vec::with_capacity(raw.len() + 1);
    let mut pc = 0;
    for i in raw {
        pcs.push(pc);
        pc += 1 + CompactCode::shape(i)?.3 as u32;
    }
    pcs.push(pc);
    for &original in raw {
        let mut i = original;
        for (kind, value) in instruction_spec(i.opcode)
            .expect("fixture opcode")
            .operands
            .iter()
            .zip(&mut i.operands)
        {
            if *kind == OperandKind::Pc {
                *value = pcs
                    .get(*value as usize)
                    .copied()
                    .unwrap_or_else(|| pc.saturating_add(*value));
            }
        }
        let (head, wide, packed, tail) = CompactCode::shape(&i)?;
        let mut words = vec![head];
        if wide {
            words.push(i.operands[0]);
        }
        if packed {
            words.push(i.operands[1] | i.operands[2] << 16);
        } else if tail - usize::from(wide) == 2 {
            words.extend_from_slice(&i.operands[1..]);
        } else if tail - usize::from(wide) == 1 {
            words.push(i.operands[1]);
        }
        builder.append(words.into_iter().map(Ok))?;
    }
    Ok(())
}
fn put_code_store(
    package: &BytecodePackage<impl InstructionSequence>,
    w: &mut Writer,
) -> Result<(), Error> {
    let store = w.code_store.clone().or_else(|| {
        package
            .functions
            .first()
            .and_then(|f| f.code.runtime_store())
            .cloned()
    });
    if let Some(store) = store {
        w.capacity(
            store.words_len() as usize + store.blocks().len(),
            store.cache_stats().cold_capacity_bytes,
        )?;
        store.words_len().put(w)?;
        (store.blocks().len() as u32).put(w)?;
        for block in store.blocks() {
            block.offset.put(w)?;
            block.len.put(w)?;
        }
        (store.packed().len() as u32).put(w)?;
        w.write(store.packed())?;
    } else {
        // Editable IR is measured before verification. Use a conservative wire
        // bound without compressing or allocating a second copy of its code.
        let words = package.functions.iter().try_fold(0usize, |n, f| {
            f.code.iter().try_fold(n, |n, i| {
                n.checked_add(1 + CompactCode::shape(&i?)?.3)
                    .ok_or_else(|| Error::new(ErrorKind::Limit, "code words"))
            })
        })?;
        let blocks = words.div_ceil(PAGE_WORDS);
        (words as u32).put(w)?;
        (blocks as u32).put(w)?;
        for _ in 0..blocks {
            0u32.put(w)?;
            0u32.put(w)?;
        }
        let upper = words
            .checked_mul(4)
            .and_then(|n| n.checked_add(blocks * 128 + words / 250))
            .ok_or_else(|| Error::new(ErrorKind::Limit, "compressed code bound"))?;
        (upper as u32).put(w)?;
        // Measuring has neither an output vector nor a digest.
        debug_assert!(w.bytes.is_none() && w.digest.is_none());
        w.len = w
            .len
            .checked_add(upper)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "encoded size"))?;
        if w.len > w.max || w.len as u64 > w.limits.max_work {
            return Err(Error::new(ErrorKind::Limit, "encoded byte/work limit"));
        }
    }
    Ok(())
}
fn get_code_store(r: &mut Reader<'_>) -> Result<(), Error> {
    let words = u32::get(r)?;
    let blocks = r.count::<CodeBlock>()?;
    if blocks != (words as usize).div_ceil(PAGE_WORDS) {
        return Err(r.error(ErrorKind::Encoding, "execution page count"));
    }
    let mut directory = Vec::new();
    if r.code_store.is_none() {
        directory
            .try_reserve_exact(blocks)
            .map_err(|_| r.error(ErrorKind::Allocation, "execution page directory"))?;
    }
    for _ in 0..blocks {
        let offset = u32::get(r)?;
        let len = u32::get(r)?;
        if r.code_store.is_none() {
            directory.push(CodeBlock { offset, len });
        }
    }
    let packed = u32::get(r)? as usize;
    r.charge(0, packed)?;
    let scratch = if words == 0 {
        0
    } else {
        CodeStore::construction_scratch_bytes()
    };
    let allocation = CodeStore::planned_capacity_bytes(words, packed, blocks)?
        .checked_add(scratch)
        .ok_or_else(|| r.error(ErrorKind::Limit, "execution construction capacity"))?;
    r.charge(
        words,
        allocation.saturating_sub(packed + blocks * size_of::<CodeBlock>()),
    )?;
    r.work = r
        .work
        .checked_add(u64::from(words) * 4)
        .ok_or_else(|| r.error(ErrorKind::Limit, "execution scan work"))?;
    if r.work > r.limits.max_work {
        return Err(r.error(ErrorKind::Limit, "execution scan work"));
    }
    let input = r.input;
    let start = r.pos;
    r.take(packed)?;
    if r.code_store.is_none() {
        let mut data = Vec::new();
        data.try_reserve_exact(packed)
            .map_err(|_| r.error(ErrorKind::Allocation, "compressed execution words"))?;
        data.extend_from_slice(&input[start..start + packed]);
        r.code_store = Some(CodeStore::from_blocks(words, data, directory)?);
    }
    Ok(())
}
impl Wire for CodeBlock {
    const MIN: usize = 8;
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        self.offset.put(w)?;
        self.len.put(w)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            offset: u32::get(r)?,
            len: u32::get(r)?,
        })
    }
}
fn put_package<C: InstructionSequence>(
    code: &BytecodePackage<C>,
    w: &mut Writer,
) -> Result<(), Error> {
    code.text_pool.put(w)?;
    code.symbols.put(w)?;
    code.types.put(w)?;
    code.constants.put(w)?;
    code.signatures.put(w)?;
    code.natives.put(w)?;
    code.modules.put(w)?;
    put_code_store(code, w)?;
    w.capacity(
        code.functions.len(),
        code.functions.capacity() * size_of::<FunctionDef<C>>(),
    )?;
    u32::try_from(code.functions.len())
        .map_err(|_| Error::new(ErrorKind::Limit, "function count"))?
        .put(w)?;
    for function in &code.functions {
        put_function(function, w)?;
    }
    code.capabilities.put(w)?;
    code.sources.put(w)
}

impl Wire for RuntimePackage {
    fn put(&self, w: &mut Writer) -> Result<(), Error> {
        put_package(self, w)
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
            functions: {
                get_code_store(r)?;
                let functions = r.expected(r.counts[6])?;
                if r.function_word != r.code_store.as_ref().expect("code store").words_len() {
                    return Err(r.error(ErrorKind::Encoding, "unreferenced execution words"));
                }
                functions
            },
            capabilities: r.expected(r.counts[7])?,
            sources: r.expected(r.counts[8])?,
            ..Self::default()
        })
    }
}

fn counts(p: &BytecodePackage<impl InstructionSequence>) -> Result<[u32; 9], Error> {
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
pub fn encode(
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    let stats = super::verify::validate(package, limits)?;
    // Validation's derived cache has been dropped; only the package remains
    // resident while encoding allocates its output buffer.
    encode_fields(
        package,
        limits,
        stats.owned_capacity_bytes - stats.derived_capacity_bytes,
    )
}
/// Serialize the immutable verifier result without rebuilding semantic metadata.
pub(super) fn encode_verified(program: &VerifiedProgram, limits: Limits) -> Result<Vec<u8>, Error> {
    if program
        .package()
        .functions
        .iter()
        .any(|f| f.register_count > limits.max_registers)
    {
        return Err(Error::new(ErrorKind::Limit, "register limit"));
    }
    encode_fields(
        program.package(),
        limits,
        program.stats().owned_capacity_bytes,
    )
}
fn encode_fields(
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
    owned: usize,
) -> Result<Vec<u8>, Error> {
    encode_fields_impl(
        package,
        limits,
        owned,
        #[cfg(test)]
        false,
    )
}
fn encode_fields_impl(
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
    owned: usize,
    #[cfg(test)] unverified_fixture: bool,
) -> Result<Vec<u8>, Error> {
    let borrowed = package
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
        .is_some();
    let store = prepare_code_store(
        package,
        limits,
        owned,
        #[cfg(test)]
        unverified_fixture,
    )?;
    let owned = owned
        .checked_add(if borrowed { 0 } else { store.capacity_bytes() })
        .ok_or_else(|| Error::new(ErrorKind::Limit, "encoding code capacity"))?;
    let mut measure = Writer {
        digest: None,
        bytes: None,
        len: HEADER_BYTES,
        max: limits.max_bytes,
        capacity: 0,
        retained: 0,
        #[cfg(test)]
        unverified_fixture: false,
        records: 0,
        code_store: None,
        function_word: 0,
        limits,
    };
    measure.code_store = Some(store.clone());
    put_package(package, &mut measure)?;
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
        retained: owned,
        #[cfg(test)]
        unverified_fixture,
        records: 0,
        code_store: None,
        function_word: 0,
        limits,
    };
    w.code_store = Some(store);
    w.write(MAGIC)?;
    package.format_version.put(&mut w)?;
    package.language_version.put(&mut w)?;
    package.host_abi_version.put(&mut w)?;
    package.content_version.put(&mut w)?;
    len.put(&mut w)?;
    for n in counts(package)? {
        n.put(&mut w)?;
    }
    put_package(package, &mut w)?;
    Ok(w.bytes.expect("output writer"))
}
/// Complete allocation-bounded shape/index scan, then fallible materialization.
/// Semantic verification and runtime capability checks are explicit next steps.
pub fn decode(input: &[u8], limits: Limits) -> Result<RuntimePackage, Error> {
    if input.len() > limits.max_bytes {
        return Err(Error::new(ErrorKind::Limit, "input bytes"));
    }
    let mut r = Reader::new(input, limits);
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
    let _scan = RuntimePackage::get(&mut r)?;
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
    r.function_word = 0;
    let mut package = RuntimePackage::get(&mut r)?;
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
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
) -> Result<(usize, usize, u64), Error> {
    let mut w = Writer {
        digest: None,
        bytes: None,
        len: HEADER_BYTES,
        max: if package
            .functions
            .iter()
            .any(|f| f.code.runtime_words().is_some())
        {
            limits.max_bytes
        } else {
            isize::MAX as usize
        },
        capacity: size_of::<BytecodePackage>(),
        retained: 0,
        #[cfg(test)]
        unverified_fixture: false,
        records: 0,
        code_store: None,
        function_word: 0,
        limits,
    };
    put_package(package, &mut w)?;
    Ok((w.len, w.capacity, w.records))
}
#[cfg(test)]
pub(super) fn encode_unverified(package: &BytecodePackage<impl InstructionSequence>) -> Vec<u8> {
    encode_fields_impl(package, Limits::default(), 0, true).expect("bounded malformed test fixture")
}

// Hash canonical fields without materializing another copy of the code package.
pub(crate) fn identity(
    package: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
) -> Result<[u8; 32], Error> {
    let mut writer = Writer {
        bytes: None,
        digest: Some(blake3::Hasher::new()),
        len: 0,
        max: limits.max_bytes,
        capacity: 0,
        retained: 0,
        #[cfg(test)]
        unverified_fixture: false,
        records: 0,
        code_store: None,
        function_word: 0,
        limits,
    };
    writer.code_store = Some(prepare_code_store(
        package,
        limits,
        0,
        #[cfg(test)]
        false,
    )?);
    package.format_version.put(&mut writer)?;
    package.language_version.put(&mut writer)?;
    package.host_abi_version.put(&mut writer)?;
    package.content_version.put(&mut writer)?;
    put_package(package, &mut writer)?;
    Ok(*writer.digest.unwrap().finalize().as_bytes())
}

/// Hash the same canonical fields after the decoder and semantic verifier have
/// accepted the input. The magic, total length and count directory are framing;
/// versions and every package field retain their original identity coverage.
pub(super) fn decoded_identity(input: &[u8], limits: Limits) -> Result<[u8; 32], Error> {
    let mut writer = Writer {
        bytes: None,
        digest: Some(blake3::Hasher::new()),
        len: 0,
        max: limits.max_bytes,
        capacity: 0,
        retained: 0,
        #[cfg(test)]
        unverified_fixture: false,
        records: 0,
        code_store: None,
        function_word: 0,
        limits,
    };
    writer.write(&input[MAGIC.len()..MAGIC.len() + 3 * size_of::<u32>() + size_of::<u64>()])?;
    writer.write(&input[HEADER_BYTES..])?;
    Ok(*writer.digest.unwrap().finalize().as_bytes())
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn compressed_code_precharges_construction_workspace_before_inflating() {
        let mut builder = CodeStoreBuilder::new();
        builder.append([Ok(Opcode::Return as u32)]).unwrap();
        let store = builder.finish().unwrap();
        let mut wire = Vec::new();
        for value in [
            1u32,
            1,
            0,
            store.packed().len() as u32,
            store.packed().len() as u32,
        ] {
            wire.extend_from_slice(&value.to_le_bytes());
        }
        wire.extend_from_slice(store.packed());
        let capacity = CodeStore::planned_capacity_bytes(1, store.packed().len(), 1).unwrap()
            + CodeStore::construction_scratch_bytes();
        let mut rejected = Reader::new(
            &wire,
            Limits {
                max_allocation_bytes: capacity - 1,
                ..Limits::default()
            },
        );
        let allocated = allocation_counter::measure(|| {
            assert_eq!(
                get_code_store(&mut rejected).unwrap_err().kind,
                ErrorKind::Limit
            );
        });
        assert!(rejected.code_store.is_none());
        assert!(
            allocated.bytes_max < super::super::code_residency::PAGE_BYTES as u64,
            "inflater workspace was allocated before admission"
        );
        let mut admitted = Reader::new(
            &wire,
            Limits {
                max_allocation_bytes: capacity,
                ..Limits::default()
            },
        );
        get_code_store(&mut admitted).unwrap();
        assert_eq!(admitted.allocation, capacity);
        assert_eq!(
            admitted
                .code_store
                .unwrap()
                .cache_stats()
                .resident_capacity_bytes,
            0
        );
    }
}

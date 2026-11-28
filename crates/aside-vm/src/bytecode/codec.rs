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
    shared_descriptor: bool,
    retained: usize,
    #[cfg(test)]
    unverified_fixture: bool,
    records: u64,
    code_store: Option<Arc<CodeStore>>,
    function_word: u32,
    limits: Limits,
}
impl Writer {
    fn scratch(&self, bytes: usize) -> Result<(), Error> {
        let requested = self
            .retained
            .max(self.capacity)
            .checked_add(self.bytes.as_ref().map_or(0, Vec::capacity))
            .and_then(|n| n.checked_add(bytes));
        if requested.is_none_or(|n| n > self.limits.max_allocation_bytes) {
            return Err(Error::new(
                ErrorKind::Limit,
                format!(
                    "encoding scratch capacity: requested {requested:?}, limit {}",
                    self.limits.max_allocation_bytes
                ),
            ));
        }
        Ok(())
    }
    fn unverified_fixture(&self) -> bool {
        #[cfg(test)]
        {
            self.unverified_fixture
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn capacity(&mut self, records: usize, bytes: usize) -> Result<(), Error> {
        let bytes = if self.shared_descriptor { 0 } else { bytes };
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
    descriptor_interner: DescriptorInterner,
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
            descriptor_interner: DescriptorInterner::default(),
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
        <Vec<T> as Wire>::get(self)
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
        put_vec_with(self, w, |value, writer| value.put(writer))
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        counted_vec(r).map(|(values, _)| values)
    }
}
fn put_vec_with<T: Wire>(
    values: &Vec<T>,
    w: &mut Writer,
    mut put: impl FnMut(&T, &mut Writer) -> Result<(), Error>,
) -> Result<(), Error> {
    w.capacity(
        values.len(),
        values
            .capacity()
            .checked_mul(size_of::<T>())
            .ok_or_else(|| Error::new(ErrorKind::Limit, "vector capacity"))?,
    )?;
    u32::try_from(values.len())
        .map_err(|_| Error::new(ErrorKind::Limit, "vector length"))?
        .put(w)?;
    for value in values {
        put(value, w)?;
    }
    Ok(())
}
fn counted_vec<T: Wire>(r: &mut Reader<'_>) -> Result<(Vec<T>, usize), Error> {
    counted_vec_with(r, T::get)
}
fn counted_vec_with<T: Wire>(
    r: &mut Reader<'_>,
    mut get: impl FnMut(&mut Reader<'_>) -> Result<T, Error>,
) -> Result<(Vec<T>, usize), Error> {
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
        let x = get(r)?;
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

mod records;
pub(super) use records::SOURCE_COMPRESSION_SCRATCH;
#[cfg(test)]
use records::compressed_source;
fn get_source_map(r: &mut Reader<'_>, expected: u32) -> Result<CompactSourceMap, Error> {
    let count = u32::get(r)?;
    if count != expected {
        return Err(r.error(ErrorKind::Encoding, "source map count mismatch"));
    }
    if count == 0 {
        return Ok(CompactSourceMap::default());
    }
    let len = u32::get(r)? as usize;
    r.charge(
        count,
        CompactSourceMap::planned_capacity(count as usize, len)?,
    )?;
    let materialize = r.materialize;
    let bytes = r.take(len)?;
    if materialize {
        CompactSourceMap::from_encoded(bytes, count)
    } else {
        CompactSourceMap::scan(bytes, count)?;
        Ok(CompactSourceMap::default())
    }
}
fn put_function(
    code: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    w: &mut Writer,
) -> Result<(), Error> {
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
    put_descriptors(&code.descriptors, w)?;
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
    if code.code.runtime_words().is_some()
        || (w.bytes.is_none() && w.digest.is_none() && w.code_store.is_none())
    {
        put_pc_metadata(code, w, Ok)?;
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
        let unverified = w.unverified_fixture();
        put_pc_metadata(code, w, |target| encode_pc(&pcs, target, unverified))?;
    }
    Ok(())
}

fn put_descriptors(values: &impl DescriptorSequence, w: &mut Writer) -> Result<(), Error> {
    w.capacity(values.len(), values.capacity_bytes())?;
    (values.len() as u32).put(w)?;
    w.shared_descriptor = values.shared();
    for value in values.iter() {
        value.put(w)?;
    }
    w.shared_descriptor = false;
    Ok(())
}

fn get_descriptors(r: &mut Reader<'_>, expected: u32) -> Result<SharedDescriptors, Error> {
    let count = u32::get(r)?;
    if count != expected {
        return Err(r.error(ErrorKind::Encoding, "descriptor count mismatch"));
    }
    let slots = (count as usize)
        .checked_mul(size_of::<Arc<Descriptor>>())
        .ok_or_else(|| r.error(ErrorKind::Limit, "descriptor slot capacity"))?;
    r.charge(count, slots)?;
    let mut out = SharedDescriptors::default();
    if r.materialize {
        out.entries
            .try_reserve_exact(count as usize)
            .map_err(|_| r.error(ErrorKind::Allocation, "descriptor slots"))?;
    }
    for _ in 0..count {
        let before = r.allocation;
        let value = Descriptor::get(r)?;
        if !r.materialize {
            // Payloads are scanned without allocation. Materialization admits each
            // unique object and its interner scratch before retaining it.
            r.allocation = before;
            continue;
        }
        if let Some(shared) = r.descriptor_interner.get(&value) {
            r.allocation = before;
            out.entries.push(shared);
        } else {
            r.charge(0, size_of::<Descriptor>() + 2 * size_of::<usize>())?;
            let scratch_before = r.descriptor_interner.scratch_bytes();
            let scratch_peak = r.descriptor_interner.insertion_scratch();
            r.charge(0, scratch_peak)?;
            let shared = r.descriptor_interner.insert(value)?;
            let scratch_after = r.descriptor_interner.scratch_bytes();
            r.allocation -= scratch_peak + scratch_before - scratch_after;
            out.entries.push(shared);
        }
    }
    Ok(out)
}
fn encode_pc(pcs: &[u32], target: Pc, unverified: bool) -> Result<Pc, Error> {
    if let Some(&pc) = pcs.get(target.index()) {
        return Ok(Pc(pc));
    }
    #[cfg(test)]
    if unverified {
        return Ok(Pc(pcs
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(target.0)));
    }
    let _ = unverified;
    Err(Error::new(ErrorKind::Index, "encoding PC"))
}
fn put_pc_metadata(
    code: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    w: &mut Writer,
    map: impl Fn(Pc) -> Result<Pc, Error>,
) -> Result<(), Error> {
    w.capacity(
        code.regions.len(),
        code.regions.capacity() * size_of::<ExceptionRegion>(),
    )?;
    (code.regions.len() as u32).put(w)?;
    for region in &code.regions {
        let mut region = region.clone();
        super::compact::map_metadata_pcs(std::slice::from_mut(&mut region), &mut [], &map)?;
        region.put(w)?;
    }
    w.capacity(code.source_map.len(), code.source_map.capacity_bytes())?;
    (code.source_map.len() as u32).put(w)?;
    if code.source_map.is_empty() {
        return Ok(());
    }
    if let Some(bytes) = code.source_map.packed() {
        (bytes.len() as u32).put(w)?;
        w.write(bytes)?;
    } else {
        let mut size = 0usize;
        let entries = || {
            code.source_map.entries().map(|mut entry| {
                entry.pc = map(entry.pc)?;
                Ok(entry)
            })
        };
        CompactSourceMap::encode_results(entries(), |part| {
            size += part.len();
            Ok(())
        })?;
        u32::try_from(size)
            .map_err(|_| Error::new(ErrorKind::Limit, "source map bytes"))?
            .put(w)?;
        CompactSourceMap::encode_results(entries(), |part| w.write(part))?;
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
            descriptors: get_descriptors(r, lens[2])?,
            code: CompactCode::get(r)?,
            regions: r.expected(lens[4])?,
            source_map: get_source_map(r, lens[5])?,
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
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
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
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
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
pub(super) fn shared_descriptor_capacity(
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
    mut scratch: impl FnMut(usize) -> Result<(), Error>,
) -> Result<usize, Error> {
    let mut seen = std::collections::HashSet::new();
    let mut retained = 0usize;
    for function in &package.functions {
        if !function.descriptors.shared() {
            continue;
        }
        for descriptor in function.descriptors.iter() {
            let identity = std::ptr::from_ref(descriptor) as usize;
            if seen.contains(&identity) {
                continue;
            }
            if seen.len() == seen.capacity() {
                let old_buckets = if seen.capacity() == 0 {
                    0
                } else {
                    (seen.capacity() + 1).next_power_of_two()
                };
                let new_buckets = (old_buckets * 2).max(4);
                scratch((old_buckets + new_buckets) * (size_of::<usize>() + 1) + 32)?;
                seen.try_reserve(1)
                    .map_err(|_| Error::new(ErrorKind::Allocation, "descriptor ownership index"))?;
            }
            seen.insert(identity);
            retained = retained
                .checked_add(super::descriptors::descriptor_bytes(descriptor))
                .ok_or_else(|| Error::new(ErrorKind::Limit, "shared descriptor capacity"))?;
        }
    }
    Ok(retained)
}

fn put_package<C: InstructionSequence, S: SourceMapSequence, D: DescriptorSequence>(
    code: &BytecodePackage<C, S, D>,
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
        0,
        shared_descriptor_capacity(code, |bytes| w.scratch(bytes))?,
    )?;
    w.capacity(
        code.functions.len(),
        code.functions.capacity() * size_of::<FunctionDef<C, S, D>>(),
    )?;
    u32::try_from(code.functions.len())
        .map_err(|_| Error::new(ErrorKind::Limit, "function count"))?
        .put(w)?;
    for function in &code.functions {
        put_function(function, w)?;
    }
    code.capabilities.put(w)?;
    // Count every resident source allocation before the first compressor starts.
    // Later source files already belong to the package at this point.
    w.capacity(
        code.sources.len(),
        code.sources.capacity() * size_of::<SourceFile>(),
    )?;
    w.capacity(
        code.sources.len(),
        code.sources.iter().try_fold(0usize, |n, source| {
            n.checked_add(source.text.capacity())
                .ok_or_else(|| Error::new(ErrorKind::Limit, "source capacity"))
        })?,
    )?;
    u32::try_from(code.sources.len())
        .map_err(|_| Error::new(ErrorKind::Limit, "source count"))?
        .put(w)?;
    for source in &code.sources {
        source.put(w)?;
    }
    Ok(())
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

fn counts(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
) -> Result<[u32; 9], Error> {
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
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    let stats = super::verify::validate(package, limits)?;
    // Validation's derived cache has been dropped; only the package remains
    // resident while encoding allocates its output buffer.
    encode_fields(
        package,
        limits,
        stats.owned_capacity_bytes - stats.derived_capacity_bytes,
        None,
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
    let stats = program.stats();
    let package = program.package();
    // The shared store is already in canonical wire order. Its final
    // measurement includes the compressed code and all encoded records.
    let measured = package
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
        .map(|_| (stats.encoded_bytes, stats.records));
    encode_fields(package, limits, stats.owned_capacity_bytes, measured)
}
fn encode_fields(
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
    limits: Limits,
    owned: usize,
    measured: Option<(usize, u64)>,
) -> Result<Vec<u8>, Error> {
    encode_fields_impl(
        package,
        limits,
        owned,
        measured,
        #[cfg(test)]
        false,
    )
}
fn encode_fields_impl(
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
    limits: Limits,
    owned: usize,
    measured: Option<(usize, u64)>,
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
    let wire_bytes = if let Some((bytes, records)) = measured {
        if bytes > limits.max_bytes || bytes as u64 > limits.max_work {
            return Err(Error::new(ErrorKind::Limit, "encoded byte/work limit"));
        }
        if records > u64::from(limits.max_records) {
            return Err(Error::new(ErrorKind::Limit, "encoded record limit"));
        }
        bytes
    } else {
        let mut measure = Writer {
            digest: None,
            bytes: None,
            len: HEADER_BYTES,
            max: limits.max_bytes,
            capacity: 0,
            shared_descriptor: false,
            retained: owned,
            #[cfg(test)]
            unverified_fixture,
            records: 0,
            code_store: Some(store.clone()),
            function_word: 0,
            limits,
        };
        put_package(package, &mut measure)?;
        measure.len
    };
    let len =
        u32::try_from(wire_bytes).map_err(|_| Error::new(ErrorKind::Limit, "package size"))?;
    if owned
        .checked_add(wire_bytes)
        .is_none_or(|n| n > limits.max_allocation_bytes)
    {
        return Err(Error::new(
            ErrorKind::Limit,
            "package/output allocation limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(wire_bytes)
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
        shared_descriptor: false,
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
    // Reuse the count directory; materialization admits each unique payload.
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
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
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
        shared_descriptor: false,
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
pub(super) fn encode_unverified(
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
) -> Vec<u8> {
    encode_fields_impl(package, Limits::default(), 0, None, true)
        .expect("bounded malformed test fixture")
}

// Hash canonical fields without materializing another copy of the code package.
pub(crate) fn identity(
    package: &BytecodePackage<
        impl InstructionSequence,
        impl SourceMapSequence,
        impl DescriptorSequence,
    >,
    limits: Limits,
) -> Result<[u8; 32], Error> {
    let mut writer = Writer {
        bytes: None,
        digest: Some(blake3::Hasher::new()),
        len: 0,
        max: limits.max_bytes,
        capacity: 0,
        shared_descriptor: false,
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
        shared_descriptor: false,
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
mod admission_tests;

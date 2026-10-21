//! Trusted nominal host identities, shared by all objects using one code lease.
use crate::{Heap, HeapBudgetUsage, HeapError, HeapId, Value, ObjectKind,
    bytecode::{BytecodePackage, TypeDef, TypeId}, executor::VmError,
    root_ranges::{ChargedVec, Reservation}};
use std::num::NonZeroU32;

/// A host-owned nominal declaration, independent of the bytecode being loaded.
/// `name` is canonical `module::Type`; bases name other registered host types.
/// The declaration version identifies the host contract, while B01 Named types
/// retain the package content version. Register names once before execution.
#[derive(Clone, Debug)]
pub struct HostTypeSupport {
    pub name: String,
    pub version: u64,
    pub bases: Vec<String>,
}

/// A heap-local credential issued from its trusted nominal type registry.
/// Cache it in the host and attach it while constructing the corresponding value.
/// A token from another heap is rejected; script values cannot manufacture one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostTypeId {
    heap: HeapId,
    index: NonZeroU32,
}

struct Entry {
    name: String,
    accepts: ChargedVec<TypeId>,
}
pub(crate) struct HostTypes {
    entries: ChargedVec<Entry>,
    nominal: ChargedVec<TypeId>,
    _names: Reservation,
    pub fingerprint: [u8; 32],
    pub work: u64,
}
impl HostTypes {
    pub fn new(package: &BytecodePackage, registrations: &[HostTypeSupport], usage: &HeapBudgetUsage) -> Result<Self, VmError> {
        // Zero means untyped. All encoded nonzero ids fit u32, so shifting the
        // credential left once always fits a positive i64 without truncation.
        if registrations.len() > u32::MAX as usize {return Err(VmError::Limit);}
        if registrations.is_empty() {
            return Ok(Self {entries:ChargedVec::new(usage,0)?,nominal:ChargedVec::new(usage,0)?,
                _names:Reservation::new(usage,0)?,fingerprint:*blake3::hash(&[]).as_bytes(),work:0});
        }
        let mut order = ChargedVec::new(usage, registrations.len())?;
        order.data.extend(0..registrations.len());
        order.data.sort_unstable_by(|&a,&b| registrations[a].name.cmp(&registrations[b].name));
        let find = |name: &str| order.data.binary_search_by(|&i| registrations[i].name.as_str().cmp(name)).ok();
        let mut names_bytes = 0u64;
        let mut fingerprint = blake3::Hasher::new();
        for (index,&id) in order.data.iter().enumerate() {
            let declaration = &registrations[id];
            if declaration.version == 0 || declaration.name.rsplit_once("::").is_none_or(|(module,name)| module.is_empty() || name.is_empty())
                || index > 0 && registrations[order.data[index-1]].name == declaration.name {
                return Err(VmError::Contract("invalid or duplicate trusted host type"));
            }
            names_bytes = names_bytes.checked_add(declaration.name.len() as u64).ok_or(VmError::Limit)?;
            fingerprint.update(&(declaration.name.len() as u64).to_le_bytes());
            fingerprint.update(declaration.name.as_bytes());
            fingerprint.update(&declaration.version.to_le_bytes());
            fingerprint.update(&(declaration.bases.len() as u64).to_le_bytes());
            for (i,base) in declaration.bases.iter().enumerate() {
                if find(base).is_none() || declaration.bases[..i].contains(base) {
                    return Err(VmError::Contract("unknown or duplicate trusted host base"));
                }
                fingerprint.update(&(base.len() as u64).to_le_bytes());
                fingerprint.update(base.as_bytes());
            }
        }
        let names = Reservation::new(usage, names_bytes)?;
        let mut owners = ChargedVec::new(usage, package.types.len())?;
        for ty in &package.types {
            let owner = if let TypeDef::Named {module,name,..} = ty {
                let path = &package.symbols[package.modules[module.index()].path.index()];
                let name = &package.symbols[name.index()];
                order.data.iter().position(|&id| registrations[id].name.rsplit_once("::") == Some((path.as_str(),name.as_str())))
            } else {None};
            owners.data.push(owner.map_or(u32::MAX,|index|index as u32));
        }
        let mut nominal = ChargedVec::new(usage,owners.data.iter().filter(|&&owner|owner != u32::MAX).count())?;
        nominal.data.extend(owners.data.iter().enumerate().filter_map(|(index,&owner)|(owner != u32::MAX).then_some(TypeId(index as u32))));
        let mut visited = ChargedVec::new(usage, registrations.len())?;
        visited.data.resize(registrations.len(),false);
        let mut pending = ChargedVec::new(usage, registrations.len())?;
        let mut entries = ChargedVec::new(usage, registrations.len())?;
        let mut max_accepts = 0usize;
        for start in 0..order.data.len() {
            visited.data.fill(false);pending.data.clear();
            visited.data[start]=true;pending.data.push(start);
            while let Some(index) = pending.data.pop() {
                for base in &registrations[order.data[index]].bases {
                    let base = find(base).expect("validated trusted base");
                    if base == start {return Err(VmError::Contract("cyclic trusted host inheritance"));}
                    if !visited.data[base] {visited.data[base]=true;pending.data.push(base);}
                }
            }
            let count = owners.data.iter().filter(|&&owner|owner != u32::MAX && visited.data[owner as usize]).count();
            let mut accepts = ChargedVec::new(usage,count)?;
            accepts.data.extend(owners.data.iter().enumerate().filter_map(|(i,&owner)|
                (owner != u32::MAX && visited.data[owner as usize]).then_some(TypeId(i as u32))));
            max_accepts=max_accepts.max(count);
            entries.data.push(Entry {name: registrations[order.data[start]].name.clone(),accepts});
        }
        let work=max_accepts.max(nominal.data.len()).checked_ilog2().map_or(0,|value|u64::from(value+1));
        Ok(Self {entries, nominal, _names:names, fingerprint:*fingerprint.finalize().as_bytes(), work})
    }
    pub fn contains(&self,ty:TypeId)->bool {self.nominal.data.binary_search(&ty).is_ok()}
    pub fn valid_index(&self,index:u32)->bool {index == 0 || index as usize <= self.entries.data.len()}
    fn accepts(&self,index:NonZeroU32,ty:TypeId)->Result<bool,HeapError> {
        let entry=self.entries.data.get(index.get() as usize-1).ok_or(HeapError::TypeMismatch)?;
        Ok(entry.accepts.data.binary_search(&ty).is_ok())
    }
}

impl Heap {
    /// Resolve a canonical registered host type once for this heap.
    pub fn host_type(&self,name:&str)->Result<HostTypeId,HeapError> {
        let code=self.code.as_ref().ok_or(HeapError::TypeMismatch)?;
        let index=code.checks.host_types.entries.data.binary_search_by(|entry|entry.name.as_str().cmp(name)).map_err(|_|HeapError::TypeMismatch)?;
        let index=NonZeroU32::new(u32::try_from(index+1).map_err(|_|HeapError::TypeMismatch)?).unwrap();
        Ok(HostTypeId{heap:self.id,index})
    }
    /// Attach the real, immutable nominal identity while constructing a host.
    /// Reapplying the same credential is allowed; changing its type is rejected.
    pub fn set_host_type(&mut self,host:Value,ty:HostTypeId)->Result<(),HeapError> {
        if ty.heap != self.id {return Err(HeapError::WrongHeap);}
        self.host_fields(host)?;
        let flags=self.get_field(host,1)?.as_int().ok_or(HeapError::WrongKind)?;
        let current=flags>>1;
        if current != 0 && current != i64::from(ty.index.get()) {return Err(HeapError::ReadOnly);}
        self.set_field(host,1,Value::int((i64::from(ty.index.get())<<1)|(flags&1)))
    }
    pub(crate) fn accepts_host_nominal(&self,host:Value,ty:TypeId)->Result<bool,HeapError> {
        let flags=self.get_field(host,1)?.as_int().ok_or(HeapError::WrongKind)?;
        let index=u32::try_from(flags>>1).map_err(|_|HeapError::TypeMismatch)?;
        let Some(index)=NonZeroU32::new(index) else {return Ok(false);};
        self.code.as_ref().ok_or(HeapError::TypeMismatch)?.checks.host_types.accepts(index,ty)
    }
    pub(crate) fn validate_host_flags(&self,flags:Value)->Result<(),HeapError> {
        let flags=flags.as_int().filter(|&flags|flags>=0).ok_or(HeapError::TypeMismatch)?;
        let index=u32::try_from(flags>>1).map_err(|_|HeapError::TypeMismatch)?;
        if index == 0 || self.code.as_ref().is_some_and(|code|code.checks.host_types.valid_index(index)) {Ok(())}
        else {Err(HeapError::TypeMismatch)}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RootOwner,RootKind,SessionEpoch,GcBudget,GcPhase,
        executor::{Vm,VmLimits},bytecode::*,snapshot::*};

    fn program()->VerifiedProgram {
        verify(BytecodePackage {
            content_version:7,
            symbols:["sample","Node","Text","Image","Local"].map(str::to_owned).to_vec(),
            types:std::iter::once(TypeDef::Builtin(BuiltinType::Any)).chain((1..5).map(|name|
                TypeDef::Named {module:ModuleId(0),name:SymbolId(name),version:7})).collect(),
            modules:vec![ModuleDef {path:SymbolId(0),imports:vec![],exports:vec![],globals:vec![],initializer:None}],
            ..Default::default()
        },Limits::default()).unwrap()
    }
    fn registration()->Vec<HostTypeSupport> {
        vec![HostTypeSupport {name:"sample::Text".into(),version:1,bases:vec!["sample::Node".into()]},
            HostTypeSupport {name:"sample::Node".into(),version:1,bases:vec![]},
            HostTypeSupport {name:"sample::Image".into(),version:1,bases:vec!["sample::Node".into()]}]
    }
    fn support(types:Vec<HostTypeSupport>)->RuntimeSupport {RuntimeSupport {host_types:types,..Default::default()}}
    fn accepts(vm:&Vm,ty:u32,value:Value)->bool {
        vm.heap().code.as_ref().unwrap().checks.accepts(vm.program().package(),vm.heap(),TypeId(ty),value).unwrap()
    }
    #[test]
    fn nominal_hosts_share_credentials_without_per_object_storage_or_check_allocations() {
        let mut vm=Vm::load(program(),support(registration()),VmLimits::default(),SessionEpoch(1)).unwrap();
        let text=vm.heap().host_type("sample::Text").unwrap();
        let image=vm.heap().host_type("sample::Image").unwrap();
        let fields=vm.heap_mut().alloc_table().unwrap();
        let host=vm.heap_mut().alloc_composable_host(fields,&[Value::int(42)],None).unwrap();
        let before=vm.heap().stats();
        vm.heap_mut().set_host_type(host,text).unwrap();
        let after=vm.heap().stats();
        assert_eq!((before.live_objects,before.payload_bytes,before.charged_bytes),(after.live_objects,after.payload_bytes,after.charged_bytes));
        assert!(vm.heap().is_composable_host(host).unwrap());
        assert_eq!(vm.heap().host_payload_len(host).unwrap(),1);
        assert_eq!(vm.heap().host_payload(host,0).unwrap().as_int(),Some(42));
        assert!(accepts(&vm,1,host));assert!(accepts(&vm,2,host));assert!(!accepts(&vm,3,host));assert!(!accepts(&vm,4,host));
        assert_eq!(vm.heap_mut().set_host_type(host,image),Err(HeapError::ReadOnly));
        let mut foreign=Vm::load(program(),support(registration()),VmLimits::default(),SessionEpoch(2)).unwrap();
        let fields=foreign.heap_mut().alloc_table().unwrap();
        let other=foreign.heap_mut().alloc_host(fields,&[],None).unwrap();
        assert_eq!(foreign.heap_mut().set_host_type(other,text),Err(HeapError::WrongHeap));
        assert!(!accepts(&foreign,1,other),"an unmarked Host cannot claim a package name");
        let measured=allocation_counter::measure(|| {
            for _ in 0..10_000 {assert!(accepts(&vm,1,host));assert!(accepts(&vm,2,host));assert!(!accepts(&vm,3,host));}
        });
        assert_eq!(measured.count_total,0);
        eprintln!("30000 host nominal checks, allocations=0; assigning credential adds no objects or payload bytes");
        let root=vm.heap_mut().add_root(RootOwner(1),RootKind::Host,host).unwrap();
        vm.begin_collection().unwrap();
        while vm.heap().phase()!=GcPhase::Idle {vm.gc_step(GcBudget{units:1,..Default::default()}).unwrap();}
        assert!(accepts(&vm,2,host));
        vm.heap_mut().retire_host(host).unwrap();
        assert!(accepts(&vm,2,host),"retirement retains nominal identity");
        assert_eq!(vm.heap().host_fields(host),Err(HeapError::RetiredHost));
        assert_eq!(vm.heap().host_payload(host,0).unwrap(),Value::NULL);
        vm.heap_mut().release_root(root).unwrap();
    }
    #[test]
    fn host_registration_validates_inheritance_and_rejects_forged_instance_claims() {
        let mut vm=Vm::load(program(),support(registration()),VmLimits::default(),SessionEpoch(1)).unwrap();
        let bits=vm.heap_mut().alloc_array(&[Value::int((1<<1)|(1<<4))]).unwrap();
        let mut class=[Value::NULL;9];class[crate::class::ClassSlot::Types as usize]=bits;
        let class=vm.heap_mut().alloc_record(ObjectKind::Class,&class).unwrap();
        let instance=vm.heap_mut().alloc_record(ObjectKind::Instance,&[class,Value::NULL,Value::int(1),Value::NULL]).unwrap();
        assert!(!accepts(&vm,1,instance),"script class bitsets do not confer a host credential");
        assert!(accepts(&vm,4,instance),"ordinary script nominal checks retain their bitset behavior");
        for mutate in [0,1,2] {
            let mut types=registration();
            match mutate {0=>types.push(types[0].clone()),1=>types[1].bases.push("missing::Type".into()),_=>types[1].bases.push("sample::Text".into())}
            assert!(Vm::load(program(),support(types),VmLimits::default(),SessionEpoch(2)).is_err());
        }
    }
    #[test]
    fn nominal_host_snapshots_bind_to_the_trusted_registry_and_restore_new_heap_tokens() {
        let mut vm=Vm::load(program(),support(registration()),VmLimits::default(),SessionEpoch(1)).unwrap();
        let token=vm.heap().host_type("sample::Text").unwrap();
        let fields=vm.heap_mut().alloc_table().unwrap();
        let host=vm.heap_mut().alloc_host(fields,&[],None).unwrap();
        vm.heap_mut().set_host_type(host,token).unwrap();
        let root=vm.heap_mut().add_root(RootOwner(1),RootKind::Host,host).unwrap();
        let registry=SnapshotRegistry::default();
        let mut capture=vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget{units:1}).unwrap().complete {}
        let snapshot=capture.finish().unwrap();
        let bytes=snapshot.encode().unwrap();let snapshot=VmSnapshot::decode(&bytes).unwrap();
        let mut reordered=registration();reordered.reverse();
        let mut restore=Vm::restore_begin(&snapshot,program(),support(reordered),VmLimits::default(),SessionEpoch(2),&registry).unwrap();
        while !restore.step(SnapshotBudget{units:1}).unwrap().complete {}
        let (mut restored,map)=restore.finish().unwrap();
        let host=restored.heap().root_value(map.root(root).unwrap()).unwrap();
        assert!(accepts(&restored,1,host));assert!(accepts(&restored,2,host));assert!(!accepts(&restored,3,host));
        assert_eq!(restored.heap_mut().set_host_type(host,token),Err(HeapError::WrongHeap));
        let token=restored.heap().host_type("sample::Text").unwrap();
        restored.heap_mut().set_host_type(host,token).unwrap();
        let mut changed=registration();changed[0].version+=1;
        let mut restore=Vm::restore_begin(&snapshot,program(),support(changed),VmLimits::default(),SessionEpoch(3),&registry).unwrap();
        let mut rejected=false;
        loop {match restore.step(SnapshotBudget{units:1}) {Err(_)=>{rejected=true;break},Ok(step) if step.complete=>break,_=>{}}}
        assert!(rejected,"a different host contract cannot reinterpret saved type indices");
    }
}

use super::*;
use crate::aside::data::{LogicalReference, LogicalReferences, TypedDataError};
use crate::aside::data::{NoReferences, TypedCapture, TypedDocument, TypedRestore};
use aside_vm::typed_data::{FieldKey, Identities, Registry, TypeKey};
use std::sync::Arc;

fn key(name: &str) -> TypeKey {
    TypeKey {
        module: "model".into(),
        name: name.into(),
    }
}
fn registry(vm: &mut SessionVm, class: Value, identities: Identities) -> Registry {
    let owner = vm.root_owner;
    let mut registry = Registry::new(vm.vm.heap_mut(), owner, identities).unwrap();
    let index = registry.begin_class(vm.vm.heap_mut(), class).unwrap();
    while !registry.step_class(vm.vm.heap(), index, 1).unwrap() {
        collect(vm);
    }
    registry
}
fn field(vm: &SessionVm, value: Value, name: &str) -> Value {
    let heap = vm.vm.heap();
    for index in 0..heap.instance_field_count(value).unwrap() {
        let field = heap.instance_field(value, index).unwrap();
        if field.name == name {
            return heap.cell_read(field.cell).unwrap();
        }
    }
    panic!("missing {name}")
}
fn capture(vm: &mut SessionVm, value: Value, registry: &Registry) -> Arc<TypedDocument> {
    let owner = vm.root_owner;
    let mut capture =
        TypedCapture::new(vm.vm.heap_mut(), owner, value, registry, &NoReferences).unwrap();
    while !capture.step(vm.vm.heap(), 1, 1).unwrap().0 {
        collect(vm);
    }
    let (document, charge) = capture.finish(vm.vm.heap_mut()).unwrap();
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    let decoded = Arc::new(TypedDocument::decode(&bytes).unwrap());
    drop((document, charge));
    decoded
}

const A: &str = r#"
    let calls={count:0};
    class Profile {
        let mut score:int=1;
        let mut items:array=[];
        let mut alias:array=[];
        let mut peer=null;
        fn init(self){calls.count+=1;}
        fn label(self){"A"}
    }
    fn opening(){
        let value=Profile();value.score=41;value.items=[value];
        value.alias=value.items;value.peer=value;
        persistent.write("fixture",model(value));
    }
"#;
const B: &str = r#"
    fn moved_before_class(){99}
    let calls={count:0};
    class CurrentProfile {
        let extra:table={enabled:true,values:[3,4],pair:("new",5)};
        let mut peer=null;
        let mut alias:array=[];
        let mut points:int=0;
        let mut items:array=[];
        fn init(self){calls.count+=1;throw "ordinary init replayed";}
        fn label(self){"B"}
    }
    fn inspect(value){
        if !(value is CurrentProfile) || value.label()!="B" {throw "current class";}
        if value.points!=41 || value.items!=value.alias || value.peer!=value || value.items[0]!=value {throw "data identity";}
        if !value.extra.enabled || value.extra.values[1]!=4 || value.extra.pair[0]!="new" {throw "new default";}
        if calls.count!=0 {throw "ordinary init ran";}
        stage.replace(text(value:derive(||f"{value.points}/{value.extra.values[1]}")));
        value.points=52;
        value.extra.values[1]=9;
        true
    }
    fn opening(){let value=persistent.write("current",CurrentProfile);inspect(value)}
"#;
fn renamed() -> Identities {
    let mut result = Identities::default();
    result.types.insert(key("CurrentProfile"), key("Profile"));
    result.fields.insert(
        FieldKey {
            owner: key("CurrentProfile"),
            name: "points".into(),
        },
        FieldKey {
            owner: key("Profile"),
            name: "score".into(),
        },
    );
    result
}

#[test]
fn state_graph_restores_current_renamed_class_fields_defaults_aliases_cycles_and_observation() {
    for encoded in [false, true] {
        let (_, mut old, _, value) = persistent::pending_write(A, encoded);
        let class = Registry::instance_class(old.vm.heap(), value).unwrap();
        let old_registry = registry(&mut old, class, Identities::default());
        let document = capture(&mut old, value, &old_registry);
        old_registry.release(old.vm.heap_mut()).unwrap();
        let (mut session, mut current, ticket, class) =
            persistent::pending_write_version(B, encoded, 2);
        assert_ne!(
            old.vm.program().package().content_version,
            current.vm.program().package().content_version
        );
        let current_registry = registry(&mut current, class, renamed());
        let owner = current.root_owner;
        let mut restore = TypedRestore::new(
            current.vm.heap_mut(),
            owner,
            document,
            &current_registry,
            &NoReferences,
        )
        .unwrap();
        while !restore.step(current.vm.heap_mut(), 1, 1).unwrap() {
            collect(&mut current);
        }
        let root = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(root).unwrap();
        assert_eq!(field(&current, loaded, "points").as_int(), Some(41));
        assert!(current.vm.heap().data_is_observable(loaded).unwrap());
        current
            .vm
            .respond(ticket, HostResponse::Ready(loaded))
            .unwrap();
        current.issued = None;
        loop {
            match next(&mut current, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    current
                        .dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                        .unwrap();
                    collect(&mut current);
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(current.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                    current.discard_delivery(&mut session).unwrap();
                    break;
                }
                outcome => panic!("current State methods: {outcome:?}"),
            }
        }
        models::drive(&mut current, &mut session, true);
        assert!(
            current
                .stage()
                .nodes()
                .any(|n| matches!(&n.kind,reel_ui::stage::NodeKind::Text(s) if s.as_ref()=="52/9"))
        );
        let retained = current.vm.heap().stats().roots;
        restore.release(current.vm.heap_mut()).unwrap();
        current_registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(root).unwrap();
        assert_eq!(current.vm.heap().stats().roots, retained - 3);
    }
}

#[test]
fn state_contract_rejects_changed_types_duplicate_keys_and_impure_new_defaults() {
    let (_, mut old, _, value) = persistent::pending_write(A, true);
    let class = Registry::instance_class(old.vm.heap(), value).unwrap();
    let old_registry = registry(&mut old, class, Identities::default());
    let document = capture(&mut old, value, &old_registry);
    old_registry.release(old.vm.heap_mut()).unwrap();
    for (declaration, expected) in [
        ("let mut score:string=\"wrong\";", "type changed"),
        (
            "let mut score:int=0;let added=unsafe_default();",
            "default requires",
        ),
        ("let mut score:int=0;let added:int;", "no pure data default"),
    ] {
        let code = format!(
            "fn unsafe_default(){{throw \"must not execute\";}}class Profile{{{declaration}}}fn opening(){{persistent.write(\"class\",Profile);}}"
        );
        let (_, mut vm, _, class) = persistent::pending_write(&code, true);
        let registry = registry(&mut vm, class, Identities::default());
        let owner = vm.root_owner;
        let result = TypedRestore::new(
            vm.vm.heap_mut(),
            owner,
            document.clone(),
            &registry,
            &NoReferences,
        );
        let error = match result {
            Err(e) => e,
            Ok(mut restore) => {
                let error = loop {
                    match restore.step(vm.vm.heap_mut(), 1, 1) {
                        Err(e) => break e,
                        Ok(false) => collect(&mut vm),
                        Ok(true) => panic!("incompatible State accepted"),
                    }
                };
                restore.release(vm.vm.heap_mut()).unwrap();
                error
            }
        };
        assert!(error.to_string().contains(expected), "{error}");
        registry.release(vm.vm.heap_mut()).unwrap();
    }
    let (_, mut vm, _, class) = persistent::pending_write(
        "class Profile{let first:int=1;let second:int=2;}fn opening(){persistent.write(\"class\",Profile);}",
        true,
    );
    let mut identity = Identities::default();
    for name in ["first", "second"] {
        identity.fields.insert(
            FieldKey {
                owner: key("Profile"),
                name: name.into(),
            },
            FieldKey {
                owner: key("Profile"),
                name: "reused".into(),
            },
        );
    }
    let owner = vm.root_owner;
    let mut registry = Registry::new(vm.vm.heap_mut(), owner, identity).unwrap();
    let index = registry.begin_class(vm.vm.heap_mut(), class).unwrap();
    let error = loop {
        match registry.step_class(vm.vm.heap(), index, 1) {
            Err(e) => break e,
            Ok(false) => {}
            Ok(true) => panic!("duplicate field key accepted"),
        }
    };
    assert!(error.to_string().contains("reuse"));
    registry.release(vm.vm.heap_mut()).unwrap();
}

#[test]
fn cancelling_typed_capture_and_restore_releases_roots_and_scratch() {
    let (_, mut vm, _, value) = persistent::pending_write(A, true);
    let class = Registry::instance_class(vm.vm.heap(), value).unwrap();
    let baseline = vm.vm.heap().stats().roots;
    let registry = registry(&mut vm, class, Identities::default());
    let owner = vm.root_owner;
    let mut capture_state =
        TypedCapture::new(vm.vm.heap_mut(), owner, value, &registry, &NoReferences).unwrap();
    capture_state.step(vm.vm.heap(), 1, 1).unwrap();
    capture_state.cancel(vm.vm.heap_mut()).unwrap();
    let document = capture(&mut vm, value, &registry);
    let mut restore =
        TypedRestore::new(vm.vm.heap_mut(), owner, document, &registry, &NoReferences).unwrap();
    restore.step(vm.vm.heap_mut(), 1, 1).unwrap();
    restore.release(vm.vm.heap_mut()).unwrap();
    registry.release(vm.vm.heap_mut()).unwrap();
    collect(&mut vm);
    assert_eq!(vm.vm.heap().stats().roots, baseline);
}

struct References {
    source: Option<aside_vm::HeapHandle>,
}
impl LogicalReferences for References {
    fn capture(
        &self,
        heap: &aside_vm::Heap,
        value: Value,
    ) -> Option<Result<LogicalReference, TypedDataError>> {
        (heap.handle(value).ok() == self.source).then(|| {
            Ok(LogicalReference {
                kind: "test.resource".into(),
                key: b"stable-asset".to_vec(),
            })
        })
    }
    fn restore(
        &self,
        heap: &mut aside_vm::Heap,
        reference: &LogicalReference,
    ) -> Result<Value, TypedDataError> {
        assert_eq!(reference.kind, "test.resource");
        assert_eq!(reference.key, b"stable-asset");
        let fields = heap.alloc_table()?;
        Ok(heap.alloc_host(fields, &[], None)?)
    }
}

#[test]
fn logical_reference_adapter_replaces_native_handles_and_preserves_aliases() {
    let mut heap = aside_vm::Heap::new(SessionEpoch(1), Default::default()).unwrap();
    let owner = RootOwner(7);
    let registry = Registry::new(&mut heap, owner, Identities::default()).unwrap();
    let fields = heap.alloc_table().unwrap();
    let native = heap.alloc_host(fields, &[], None).unwrap();
    let original = heap.handle(native).unwrap();
    let root = heap.alloc_array(&[native, native]).unwrap();
    heap.array_push(root, root).unwrap();
    let references = References {
        source: Some(original),
    };
    let mut capture = TypedCapture::new(&mut heap, owner, root, &registry, &references).unwrap();
    while !capture.step(&heap, 1, 1).unwrap().0 {}
    let (document, charge) = capture.finish(&mut heap).unwrap();
    registry.release(&mut heap).unwrap();
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    drop((document, charge));
    let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
    let mut current = aside_vm::Heap::new(SessionEpoch(2), Default::default()).unwrap();
    let registry = Registry::new(&mut current, owner, Identities::default()).unwrap();
    let references = References { source: None };
    let mut restore =
        TypedRestore::new(&mut current, owner, document, &registry, &references).unwrap();
    assert!(restore.value_root(&mut current, owner).is_err());
    while !restore.step(&mut current, 1, 1).unwrap() {}
    let token = restore.value_root(&mut current, owner).unwrap();
    let value = current.root_value(token).unwrap();
    let first = current.sequence_get(value, 0).unwrap();
    assert_eq!(
        current.handle(first).unwrap(),
        current
            .handle(current.sequence_get(value, 1).unwrap())
            .unwrap()
    );
    assert_ne!(current.handle(first).unwrap(), original);
    assert!(current.sequence_get(value, 2).unwrap().same_bits(value));
    restore.release(&mut current).unwrap();
    registry.release(&mut current).unwrap();
    current.release_root(token).unwrap();
}

#[test]
fn literal_defaults_are_fresh_per_instance_and_work_slices_remain_bounded() {
    let old_code =
        "class State{let value:int=1;}fn opening(){persistent.write(\"state\",[State(),State()]);}";
    let (_, mut old, _, value) = persistent::pending_write(old_code, true);
    let instance = old.vm.heap().sequence_get(value, 0).unwrap();
    let class = Registry::instance_class(old.vm.heap(), instance).unwrap();
    let old_registry = registry(&mut old, class, Identities::default());
    let document = capture(&mut old, value, &old_registry);
    old_registry.release(old.vm.heap_mut()).unwrap();
    let literal = (0..2048)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let code = format!(
        "class State{{let added:array=[{literal}];let value:int=0;}}fn opening(){{persistent.write(\"class\",State);}}"
    );
    let (_, mut current, _, class) = persistent::pending_write(&code, true);
    let registry = registry(&mut current, class, Identities::default());
    let owner = current.root_owner;
    let mut restore = TypedRestore::new(
        current.vm.heap_mut(),
        owner,
        document,
        &registry,
        &NoReferences,
    )
    .unwrap();
    let before = current.vm.heap().stats().live_objects;
    assert!(!restore.step(current.vm.heap_mut(), 0, 1).unwrap());
    assert_eq!(before, current.vm.heap().stats().live_objects);
    let mut units = 0;
    let mut largest = std::time::Duration::ZERO;
    loop {
        let start = std::time::Instant::now();
        let done = restore.step(current.vm.heap_mut(), 1, 1).unwrap();
        largest = largest.max(start.elapsed());
        units += 1;
        if done {
            break;
        }
        if units % 32 == 0 {
            collect(&mut current);
        }
    }
    let token = restore.value_root(current.vm.heap_mut(), owner).unwrap();
    let value = current.vm.heap().root_value(token).unwrap();
    let a = current.vm.heap().sequence_get(value, 0).unwrap();
    let b = current.vm.heap().sequence_get(value, 1).unwrap();
    let left = field(&current, a, "added");
    let right = field(&current, b, "added");
    assert!(!left.same_bits(right));
    assert_eq!(current.vm.heap().sequence_len(left).unwrap(), 2048);
    assert_eq!(
        current
            .vm
            .heap()
            .sequence_get(right, 2047)
            .unwrap()
            .as_int(),
        Some(2047)
    );
    assert!(units > 8192);
    println!(
        "typed State: two 2048-value defaults; units={units}; max-unit={largest:?}; heap-peak={}",
        current.vm.heap().stats().peak_charged_bytes
    );
    restore.release(current.vm.heap_mut()).unwrap();
    registry.release(current.vm.heap_mut()).unwrap();
    current.vm.heap_mut().release_root(token).unwrap();
}

#[test]
fn large_schema_matching_yields_per_field_and_defers_the_last_field_error() {
    const FIELDS: usize = 512;
    let fields = (0..FIELDS)
        .map(|i| format!("let field_{i:04}:int={i};"))
        .collect::<String>();
    let code =
        format!("class State{{{fields}}}fn opening(){{persistent.write(\"state\",State());}}");
    let (_, mut old, _, value) = persistent::pending_write(&code, true);
    let class = Registry::instance_class(old.vm.heap(), value).unwrap();
    let old_registry = registry(&mut old, class, Identities::default());
    let owner = old.root_owner;
    let mut capture = TypedCapture::new(
        old.vm.heap_mut(),
        owner,
        value,
        &old_registry,
        &NoReferences,
    )
    .unwrap();
    while !capture.step(old.vm.heap(), 64, 64).unwrap().0 {
        collect(&mut old);
    }
    let (document, lease) = capture.finish(old.vm.heap_mut()).unwrap();
    old_registry.release(old.vm.heap_mut()).unwrap();
    let code = format!("class State{{{fields}}}fn opening(){{persistent.write(\"state\",State);}}");
    let (_, mut current, _, class) = persistent::pending_write_version(&code, true, 2);
    let current_registry = registry(&mut current, class, Identities::default());
    let owner = current.root_owner;
    let constructor = std::time::Instant::now();
    let mut restore = TypedRestore::new(
        current.vm.heap_mut(),
        owner,
        document.clone(),
        &current_registry,
        &NoReferences,
    )
    .unwrap();
    let constructor = constructor.elapsed();
    let objects = current.vm.heap().stats().live_objects;
    assert!(!restore.step(current.vm.heap_mut(), 0, 1).unwrap());
    let mut largest = std::time::Duration::ZERO;
    for _ in 0..=FIELDS {
        let start = std::time::Instant::now();
        assert!(!restore.step(current.vm.heap_mut(), 1, 1).unwrap());
        largest = largest.max(start.elapsed());
        assert_eq!(
            current.vm.heap().stats().live_objects,
            objects,
            "schema preparation must yield before allocating the instance graph"
        );
    }
    assert!(!restore.step(current.vm.heap_mut(), 1, 1).unwrap());
    assert_eq!(current.vm.heap().stats().live_objects, objects);
    assert!(!restore.step(current.vm.heap_mut(), 1, 1).unwrap());
    assert!(current.vm.heap().stats().live_objects > objects);
    restore.release(current.vm.heap_mut()).unwrap();
    current_registry.release(current.vm.heap_mut()).unwrap();
    let changed = fields.replace(
        &format!("field_{:04}:int={};", FIELDS - 1, FIELDS - 1),
        &format!("field_{:04}:string=\"changed\";", FIELDS - 1),
    );
    let code =
        format!("class State{{{changed}}}fn opening(){{persistent.write(\"state\",State);}}");
    let (_, mut changed, _, class) = persistent::pending_write_version(&code, true, 2);
    let changed_registry = registry(&mut changed, class, Identities::default());
    let owner = changed.root_owner;
    let mut restore = TypedRestore::new(
        changed.vm.heap_mut(),
        owner,
        document,
        &changed_registry,
        &NoReferences,
    )
    .expect("the constructor must not inspect the last field contract");
    let objects = changed.vm.heap().stats().live_objects;
    for _ in 0..FIELDS {
        assert!(!restore.step(changed.vm.heap_mut(), 1, 1).unwrap());
    }
    let error = restore.step(changed.vm.heap_mut(), 1, 1).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("field_0511: persistent field type changed"),
        "{error}"
    );
    assert_eq!(changed.vm.heap().stats().live_objects, objects);
    restore.release(changed.vm.heap_mut()).unwrap();
    changed_registry.release(changed.vm.heap_mut()).unwrap();
    drop(lease);
    println!(
        "typed State schema fields={FIELDS}; constructor={constructor:?}; max-unit={largest:?}; matching-units={}",
        FIELDS + 2
    );
}

use super::*;
use crate::aside::data::{NoReferences, TypedCapture, TypedDocument, TypedRestore};
use aside_vm::{
    resume::ResumeLimits,
    typed_data::{Registry, RegistryInitCursor, TypeKey},
};
use std::sync::Arc;
mod native_fields;

fn automatic(vm: &mut SessionVm) -> Registry {
    let owner = vm.root_owner;
    let table = vm.resume_table().clone();
    let mut cursor = RegistryInitCursor::new(&mut vm.vm, &table, owner).unwrap();
    assert!(!cursor.step(&mut vm.vm, 0).unwrap());
    while !cursor.step(&mut vm.vm, 1).unwrap() {
        collect(vm);
    }
    cursor.finish(vm.vm.heap_mut()).unwrap()
}
const A: &str = r#"
    use std::resume::{state,root};
    let calls={count:0};
    class Profile {
        let mut score:int=1;
        let mut items:array=[];
        let mut alias:array=[];
        let mut peer:Profile|null=null;
        fn init(self){calls.count+=1;self.score=41;self.items=[self];self.alias=self.items;self.peer=self;}
        fn label(self){"A"}
    }
    state(Profile,"profile");
    root(main,"game");
    fn main(value:Profile=model(Profile())){value}
    fn opening(){persistent.write("state",main());}
"#;
const B: &str = r#"
    use std::resume::{state,root};
    fn moved_before_class(){17}
    let calls={count:0};
    class CurrentProfile {
        let extra:table={enabled:true,values:[3,4]};
        let mut peer:CurrentProfile|null=null;
        let mut alias:array=[];
        let mut points:int=0;
        let mut items:array=[];
        fn init(self){calls.count+=1;throw "init must not repeat";}
        fn label(self){"B"}
    }
    state(CurrentProfile,"profile",fields:{points:"score"});
    root(main,"game");
    fn main(value:CurrentProfile=model(CurrentProfile())){value}
    fn inspect(value){
        if !(value is CurrentProfile)||value.label()!="B"||calls.count!=0{throw "current bootstrap class";}
        if value.points!=41||value.peer!=value||value.items!=value.alias||value.items[0]!=value{throw "State identity";}
        if !value.extra.enabled||value.extra.values[1]!=4{throw "default";}
        true
    }
    fn opening(){let loaded=persistent.write("placeholder",null);inspect(main(loaded))}
"#;

#[test]
fn state_and_root_source_and_bytecode_register_automatically_across_versions() {
    for encoded in [false, true] {
        let (_, mut old, _, value) = persistent::pending_write(A, encoded);
        assert_eq!(old.resume_table().table().version, 1);
        assert_eq!(old.resume_table().table().states.len(), 1);
        assert_eq!(
            old.resume_table().table().root.as_ref().unwrap().key.0,
            "game"
        );
        let registry = automatic(&mut old);
        let owner = old.root_owner;
        let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
        while !capture
            .step(old.vm.heap(), &registry, &NoReferences, 1, 1)
            .unwrap()
            .0
        {
            collect(&mut old);
        }
        let (document, charge) = capture.finish(old.vm.heap_mut()).unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
        drop(charge);
        registry.release(old.vm.heap_mut()).unwrap();
        let (mut session, mut current, ticket, _) =
            persistent::pending_write_version(B, encoded, 2);
        let contracts = current
            .resume_table()
            .normalized_state_contracts(source::types(), ResumeLimits::default())
            .unwrap();
        assert_eq!(
            contracts.states[0].schema.key,
            TypeKey {
                module: String::new(),
                name: "profile".into()
            }
        );
        assert!(
            contracts.states[0]
                .schema
                .fields
                .iter()
                .any(|f| f.key.name == "score")
        );
        let registry = automatic(&mut current);
        let owner = current.root_owner;
        let mut restore = TypedRestore::new(current.vm.heap_mut(), owner, document).unwrap();
        while !restore
            .step(current.vm.heap_mut(), &registry, &NoReferences, 1, 1)
            .unwrap()
        {
            collect(&mut current);
        }
        let root = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(root).unwrap();
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
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(current.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                    current.discard_delivery(&mut session).unwrap();
                    break;
                }
                other => panic!("current State bootstrap: {other:?}"),
            }
        }
        restore.release(current.vm.heap_mut()).unwrap();
        registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn state_root_build_contracts_reject_invalid_declarations_and_keep_empty_projects() {
    let ordinary = source::compile_story("fn opening(){true}", "plain", 1, "opening").unwrap();
    assert!(ordinary.resume.table().is_empty());
    for (source, expected) in [
        (
            "class S{let x:int=1;}state(S,\"same\");state(S,\"other\");",
            "multiple State",
        ),
        (
            "class S{let x:int=1;let y:int=2;}state(S,\"s\",fields:{x:\"same\",y:\"same\"});",
            "repeated key",
        ),
        (
            "class S{let x:int=1;}state(S,\"s\",fields:{missing:\"x\"});",
            "does not name",
        ),
        (
            "fn effect(){1}class S{let x:int=effect();}state(S,\"s\");",
            "default requires",
        ),
        (
            "class S{let callback:fn()->int;}state(S,\"s\");",
            "not persistent data",
        ),
        (
            "class Base{let x:int=1;}class S:Base{let y:int=2;}state(S,\"s\");",
            "base class",
        ),
        (
            "class S{let x:int=1;}state(S,\"s\");root(boot,\"game\");fn boot(saved:S){saved}",
            "default value",
        ),
        (
            "class S{let x:int=1;}root(boot,\"game\");fn boot(value:S=S()){value}",
            "no state declaration",
        ),
        (
            "class S{let x:int=1;}state(S,\"s\");root(boot,\"game\");root(boot,\"other\");fn boot(value:S=S()){value}",
            "only one Root",
        ),
        (
            "class S{}state(S,\"s\");root(boot,\"game\");fn boot(saved:S|null=null){saved}",
            "direct State type",
        ),
        (
            "class S{let x:int=1;}fn misplaced(){state(S,\"s\");}",
            "module statements",
        ),
    ] {
        let source = format!("use std::resume::{{state,root}};{source}fn opening(){{true}}");
        let error = source::compile_story(&source, "contract", 1, "opening")
            .err()
            .expect("invalid State/Root declaration");
        assert!(
            error.to_string().contains(expected),
            "expected {expected}: {error}"
        );
    }
}

#[test]
fn inherited_state_ownership_required_fields_and_registration_cancellation_are_explicit() {
    let code = r#"
        use std::resume::{state,root};
        class Base{let identity:int;fn init(self){self.identity=7;}}
        class State:Base{let mut payload:any=null;let mut values:array=[];}
        state(Base,"base",fields:{identity:"id"});state(State,"state");
        root(main,"game");fn main(value:State=State()){value}
        fn opening(){persistent.write("class",State);}
    "#;
    let (_, mut vm, _, _) = persistent::pending_write(code, true);
    let contracts = vm
        .resume_table()
        .normalized_state_contracts(source::types(), ResumeLimits::default())
        .unwrap();
    let state = contracts
        .states
        .iter()
        .find(|s| s.schema.key.name == "state")
        .unwrap();
    assert_eq!(state.required.len(), 1);
    let field = &state.schema.fields[state.required[0] as usize];
    assert_eq!(field.key.owner.name, "base");
    assert_eq!(field.key.name, "id");
    let roots = vm.vm.heap().stats().roots;
    let owner = vm.root_owner;
    let table = vm.resume_table().clone();
    let mut cursor = RegistryInitCursor::new(&mut vm.vm, &table, owner).unwrap();
    assert!(!cursor.step(&mut vm.vm, 1).unwrap());
    cursor.cancel(vm.vm.heap_mut()).unwrap();
    assert_eq!(vm.vm.heap().stats().roots, roots);
    let registry = automatic(&mut vm);
    registry.release(vm.vm.heap_mut()).unwrap();
}

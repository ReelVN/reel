use super::*;
use aside_vm::typed_data::DataType;

const SOURCE: &str = r#"
use std::resume::{state,root};
use reel::types::{Node,ImageNode,Action,Activity,Clock};
class Detail {let mut count:int=7;}
state(Detail,"detail");
class Scene {
    let mut detail:Detail;
    let mut base:Node|null=null;
    let mut image:ImageNode|null=null;
    let mut root:Node|null=null;
    let mut action:Action|null=null;
    let mut activity:Activity|null=null;
    let mut timer:Clock|null=null;
    fn init(self){self.detail=Detail();}
}
state(Scene,"scene");
root(main,"game");
fn main(value:Scene=model(Scene())){value}
fn opening(){
    let value=main();
    let sprite=image("sprite.png",x:7,width:100,height:50);
    value.base=sprite;value.image=sprite;
    value.root=stack(){sprite;};stage.replace(value.root);
    let action=tween(sprite,x:20,duration:1);
    value.action=action;
    value.activity=action.run(paused:true);
    value.timer=clock(interval:0.25,paused:true);
    persistent.write("typed-native",value);
}
"#;
fn field(vm: &SessionVm, value: Value, name: &str) -> Value {
    let heap = vm.vm.heap();
    (0..heap.instance_field_count(value).unwrap())
        .find_map(|i| {
            let field = heap.instance_field(value, i).unwrap();
            (field.name == name).then(|| heap.cell_read(field.cell).unwrap())
        })
        .unwrap()
}
fn contains_native(ty: &DataType, name: &str) -> bool {
    match ty {
        DataType::LogicalRef(canonical) => canonical == name,
        DataType::Union(items) => items.iter().any(|item| contains_native(item, name)),
        _ => false,
    }
}
#[test]
fn trusted_native_fields_keep_current_types_state_values_and_subtype_aliases_source_b01() {
    for encoded in [false, true] {
        let (_, mut old, _, value) = persistent::pending_write(SOURCE, encoded);
        let contracts = old
            .resume_table()
            .normalized_state_contracts(source::types(), ResumeLimits::default())
            .unwrap();
        let scene = contracts
            .states
            .iter()
            .find(|s| s.schema.key.name == "scene")
            .unwrap();
        for (field, canonical) in [
            ("base", "reel::types::Node"),
            ("image", "reel::types::ImageNode"),
            ("root", "reel::types::Node"),
            ("action", "reel::types::Action"),
            ("activity", "reel::types::Activity"),
            ("timer", "reel::types::Clock"),
        ] {
            let ty = &scene
                .schema
                .fields
                .iter()
                .find(|f| f.key.name == field)
                .unwrap()
                .ty;
            assert!(contains_native(ty, canonical), "{field}: {ty:?}");
        }
        let detail = &scene
            .schema
            .fields
            .iter()
            .find(|f| f.key.name == "detail")
            .unwrap()
            .ty;
        assert_eq!(
            detail,
            &DataType::State(TypeKey {
                module: String::new(),
                name: "detail".into()
            })
        );
        let registry = automatic(&mut old);
        let class = registry.class_for_instance(old.vm.heap(), value).unwrap();
        for field in &registry.schema(class).unwrap().fields {
            assert_eq!(
                field.ty,
                scene
                    .schema
                    .fields
                    .iter()
                    .find(|f| f.key == field.key)
                    .unwrap()
                    .ty
            );
        }
        let owner = old.root_owner;
        let references =
            stage::resources::SceneCapture::new(&old.stage, &old.activities, old.read_limit);
        let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
        while !capture
            .step(old.vm.heap(), &registry, &references, 1, 1)
            .unwrap()
            .0
        {}
        let (document, charge) = capture.finish(old.vm.heap_mut()).unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
        drop(charge);
        registry.release(old.vm.heap_mut()).unwrap();

        let changed = SOURCE.replace(
            "let mut count:int=7;",
            "let added:int=99;let mut count:int=7;",
        );
        let (_, mut current, _, _) = persistent::pending_write_version(&changed, encoded, 2);
        let registry = automatic(&mut current);
        let owner = current.root_owner;
        let references = stage::resources::SceneRestore::new(
            &mut current.stage,
            &mut current.activities,
            owner,
            current.read_limit,
        );
        let mut restore = TypedRestore::new(current.vm.heap_mut(), owner, document).unwrap();
        while !restore
            .step(current.vm.heap_mut(), &registry, &references, 1, 1)
            .unwrap()
        {}
        let root = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(root).unwrap();
        restore.release(current.vm.heap_mut()).unwrap();
        let image = field(&current, loaded, "image");
        assert!(image.same_bits(field(&current, loaded, "base")));
        let class = registry
            .class_for_instance(current.vm.heap(), loaded)
            .unwrap();
        let base = registry
            .schema(class)
            .unwrap()
            .fields
            .iter()
            .position(|field| field.key.name == "base")
            .unwrap() as u32;
        let fields = current.vm.heap_mut().alloc_table().unwrap();
        let forged = current.vm.heap_mut().alloc_host(fields, &[], None).unwrap();
        let error = registry
            .initialize_field(current.vm.heap_mut(), class, loaded, base, forged)
            .unwrap_err();
        assert!(
            error.to_string().contains("base") && error.to_string().contains("current field type"),
            "{error}"
        );
        assert!(image.same_bits(field(&current, loaded, "base")));
        let node = current.stage.id(current.vm.heap(), image).unwrap();
        assert_eq!(current.stage.scene.node(node).unwrap().props.x, 7.);
        let detail = field(&current, loaded, "detail");
        assert_eq!(field(&current, detail, "count").as_int(), Some(7));
        assert_eq!(field(&current, detail, "added").as_int(), Some(99));
        for name in ["root", "action", "activity", "timer"] {
            assert_eq!(
                current
                    .vm
                    .heap()
                    .info(field(&current, loaded, name))
                    .unwrap()
                    .kind,
                aside_vm::ObjectKind::Host
            );
        }
        registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn named_bytecode_records_and_script_classes_do_not_establish_native_trust() {
    let source = r#"use std::resume::state;use reel::types::Node;
        class Holder{let resource:Node;}state(Holder,"holder");fn opening(){true}"#;
    let mut story = source::compile_story(source, "forged", 1, "opening").unwrap();
    let error = story
        .resume
        .normalized_state_contracts(&[], ResumeLimits::default())
        .unwrap_err();
    assert!(
        error.detail.contains("resource") && error.detail.contains("trusted native"),
        "{error}"
    );
    let mut package = story.program.package().clone();
    let field = &mut package.functions[story.resume.table().states[0].class.function.index()]
        .descriptors[story.resume.table().states[0].class.descriptor.index()];
    let bc::Descriptor::Class(class) = field else {
        panic!()
    };
    let native = class.fields[0].ty;
    let bc::TypeDef::Named { module, .. } = package.types[native.index()] else {
        panic!()
    };
    let name = bc::SymbolId(package.symbols.len() as u32);
    package.symbols.push("UnknownResource".into());
    let unknown = bc::TypeId(package.types.len() as u32);
    package.types.push(bc::TypeDef::Named {
        module,
        name,
        version: 1,
    });
    let unused = bc::verify(package.clone(), Default::default()).unwrap();
    unused
        .verify_resume(
            story.resume.table().clone(),
            crate::aside::source::types(),
            Default::default(),
        )
        .unwrap()
        .normalized_state_contracts(source::types(), ResumeLimits::default())
        .unwrap();
    let bc::Descriptor::Class(class) = &mut package.functions
        [story.resume.table().states[0].class.function.index()]
    .descriptors[story.resume.table().states[0].class.descriptor.index()] else {
        panic!()
    };
    class.fields[0].ty = unknown;
    let program = bc::verify(package, Default::default()).unwrap();
    let checked = program
        .verify_resume(
            story.resume.table().clone(),
            crate::aside::source::types(),
            Default::default(),
        )
        .unwrap();
    let error = checked
        .normalized_state_contracts(source::types(), ResumeLimits::default())
        .unwrap_err();
    assert!(
        error.detail.contains("UnknownResource") && error.detail.contains("resource"),
        "{error}"
    );
    story.program = program;
    story.resume = checked;
    let initializer = story.initializer;
    let mut session = session();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    models::drive(&mut vm, &mut session, false);
    let owner = vm.root_owner;
    let roots = vm.vm.heap().stats().roots;
    let table = vm.resume_table().clone();
    let mut cursor = RegistryInitCursor::new(&mut vm.vm, &table, owner).unwrap();
    let error = loop {
        match cursor.step(&mut vm.vm, 1) {
            Err(error) => break error,
            Ok(false) => {}
            Ok(true) => panic!("unknown native identity accepted"),
        }
    };
    assert!(error.to_string().contains("resource"), "{error}");
    cursor.cancel(vm.vm.heap_mut()).unwrap();
    assert_eq!(vm.vm.heap().stats().roots, roots);

    let scripted = source::compile_story(
        r#"use std::resume::state;
        class Holder{let value:int=1;}state(Holder,"holder");fn opening(){true}"#,
        "forged",
        1,
        "opening",
    )
    .unwrap();
    let fake = aside_vm::HostTypeSupport {
        await_adapter: None,
        call: None,
        name: "forged::Holder".into(),
        version: 1,
        bases: vec![],
        methods: vec![],
    };
    let error = scripted
        .resume
        .normalized_state_contracts(&[fake], ResumeLimits::default())
        .unwrap_err();
    assert!(
        error.detail.contains("script class") && error.detail.contains("conflicts"),
        "{error}"
    );
}

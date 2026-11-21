use super::activities::advance_clock;
use super::*;
use crate::aside::data::{TypedCapture, TypedDocument, TypedRestore};
use aside_vm::typed_data::{Registry, RegistryInitCursor};
use std::sync::Arc;

mod random;

fn pending_resource_write(source: &str, encoded: bool) -> (Session, SessionVm, HostTicket, Value) {
    let source = source.replace(
        "persistent.write(\"capture\",s)",
        "run_flow(capture_resource_state,s)",
    );
    let source = format!(
        r#"{source}
        use std::resume::flow as resource_flow;
        fn capture_resource_state(s:Scene) {{
            resource_flow("resource.capture");
            persistent.write("capture",s)
        }}
    "#
    );
    persistent::pending_write(&source, encoded)
}

fn registry(vm: &mut SessionVm) -> Registry {
    let owner = vm.root_owner;
    let table = vm.resume_table().clone();
    let mut init = RegistryInitCursor::new(&mut vm.vm, &table, owner).unwrap();
    while !init.step(&mut vm.vm, 1).unwrap() {}
    init.finish(vm.vm.heap_mut()).unwrap()
}
fn field(vm: &SessionVm, value: Value, name: &str) -> Value {
    let heap = vm.vm.heap();
    (0..heap.instance_field_count(value).unwrap())
        .find_map(|i| {
            let f = heap.instance_field(value, i).unwrap();
            (f.name == name).then(|| heap.cell_read(f.cell).unwrap())
        })
        .unwrap()
}
const SOURCE: &str = r#"
use std::resume::{state,root}; use reel::types::{Node,ImageNode,Activity,Clock};
class Scene {
    let mut count:int=7;
    let mut sprite:ImageNode|null=null;
    let mut alias:ImageNode|null=null;
    let mut group:Node|null=null;
    let mut reference:any=null;
    let mut reference_alias:any=null;
}
state(Scene,"scene");
root(main,"game");
fn main(s:Scene=model(Scene())){s}
fn opening(){
    let s=main();
    let reference=bind(s.count);
    s.reference=[reference,reference];
    s.reference_alias=s.reference;
    s.sprite=image("sprite.png",x:reference,width:100,height:50);
    s.alias=s.sprite;
    s.group=stack(){s.sprite;};
    stage.replace(s.group);
    persistent.write("capture",s);
}
"#;
#[test]
fn image_state_and_children_share_one_graph_identity_source_asb() {
    for encoded in [false, true] {
        let (_, mut old, _, value) = pending_resource_write(SOURCE, encoded);
        let old_registry = registry(&mut old);
        let owner = old.root_owner;
        let adapter =
            stage::resources::SceneCapture::new(&old.stage, &old.activities, old.read_limit);
        let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
        while !capture
            .step(old.vm.heap(), &old_registry, &adapter, 1, 1)
            .unwrap()
            .0
        {}
        let (document, charge) = capture.finish(old.vm.heap_mut()).unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
        drop(charge);
        let changed = SOURCE.replace(
            "let mut count:int=7;",
            "let added:int=99;let mut count:int=7;",
        );
        let (_, mut current, _, _) = pending_resource_write(&changed, encoded);
        let registry = registry(&mut current);
        let owner = current.root_owner;
        let adapter = stage::resources::SceneRestore::new(
            &mut current.stage,
            &mut current.activities,
            owner,
            current.read_limit,
        );
        let mut restore = TypedRestore::new(current.vm.heap_mut(), owner, document).unwrap();
        while !restore
            .step(current.vm.heap_mut(), &registry, &adapter, 1, 1)
            .unwrap()
        {}
        let root = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(root).unwrap();
        restore.release(current.vm.heap_mut()).unwrap();

        let sprite = field(&current, loaded, "sprite");
        assert!(sprite.same_bits(field(&current, loaded, "alias")));
        let group = field(&current, loaded, "group");
        let children = current.vm.heap().host_payload(group, 1).unwrap();
        assert!(
            current
                .vm
                .heap()
                .sequence_get(children, 0)
                .unwrap()
                .same_bits(sprite)
        );
        assert!(
            current
                .vm
                .heap()
                .host_payload(sprite, 2)
                .unwrap()
                .same_bits(group)
        );
        let id = current.stage.id(current.vm.heap(), sprite).unwrap();
        assert_eq!(current.stage.scene.node(id).unwrap().props.x, 7.);
        assert_eq!(field(&current, loaded, "count").as_int(), Some(7));
        let references = field(&current, loaded, "reference");
        assert!(references.same_bits(field(&current, loaded, "reference_alias")));
        let reference = current.vm.heap().sequence_get(references, 0).unwrap();
        assert!(reference.same_bits(current.vm.heap().sequence_get(references, 1).unwrap()));
        let (source, writable) = current.vm.heap().property_ref(reference).unwrap();
        assert!(writable);
        assert_eq!(
            current.vm.heap().host_property(source).unwrap().as_int(),
            Some(7)
        );
        current
            .vm
            .heap_mut()
            .write_host_property(
                aside_vm::PropertyId {
                    host: sprite,
                    field: super::super::registry::IMAGE_PREFIX.len() as u32
                        + stage::NodeProperty::X.index() as u32,
                },
                Value::int(19),
                true,
            )
            .unwrap();
        assert_eq!(field(&current, loaded, "count").as_int(), Some(19));
        registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn tween_original_curve_delay_and_clock_phase_resume_after_adoption_source_asb() {
    let source = r#"
        use std::resume::{state,root}; use reel::types::{Node,ImageNode,Activity,Clock};
        class Scene {
            let mut sprite:ImageNode|null=null; let mut motion:Activity|null=null;
            let mut timer:Clock|null=null; let mut waiting:Activity|null=null;
        }
        state(Scene,"scene");root(main,"game");
        fn main(s:Scene=model(Scene())){s}
        fn opening(){
            let s=main();s.sprite=image("sprite.png",x:0,width:100,height:50);
            s.motion=run(tween(s.sprite,duration:10,easing:"in",x:100),owner:s.sprite,skip:"finish");
            s.timer=clock(interval:2,owner:s.sprite);
            s.waiting=run(delay(10),owner:s.sprite);
            stage.replace(s.sprite);persistent.write("capture",s);
        }
    "#;
    for encoded in [false, true] {
        let (session, mut old, _, value) = pending_resource_write(source, encoded);
        advance_clock(&mut old, &session, 3., 64);
        let old_registry = registry(&mut old);
        let owner = old.root_owner;
        let adapter =
            stage::resources::SceneCapture::new(&old.stage, &old.activities, old.read_limit);
        let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
        while !capture
            .step(old.vm.heap(), &old_registry, &adapter, 1, 1)
            .unwrap()
            .0
        {}
        let (document, charge) = capture.finish(old.vm.heap_mut()).unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
        drop(charge);
        let (mut session, mut current, _, _) = pending_resource_write(source, encoded);
        let reading = session.prepare_reading().unwrap();
        session.push_reading(reading);
        // Restore into a candidate with no previous producers, as the normal loader does.
        current.activities.revoke(current.vm.heap_mut()).unwrap();
        current.activities = activity::Activities::default();
        let registry = registry(&mut current);
        let owner = current.root_owner;
        let adapter = stage::resources::SceneRestore::new(
            &mut current.stage,
            &mut current.activities,
            owner,
            current.read_limit,
        );
        let mut restore = TypedRestore::new(current.vm.heap_mut(), owner, document).unwrap();
        while !restore
            .step(current.vm.heap_mut(), &registry, &adapter, 1, 1)
            .unwrap()
        {}
        let token = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(token).unwrap();
        restore.release(current.vm.heap_mut()).unwrap();
        while !current.prepare_checkpoint_resources(1).unwrap() {}
        assert!(!current.activities.needs_time());
        let sprite = field(&current, loaded, "sprite");
        current
            .stage
            .dispatch(
                &mut current.vm,
                owner,
                "reel::stage::replace",
                &[sprite],
                current.read_limit,
                None,
            )
            .unwrap();
        let id = current.stage.id(current.vm.heap(), sprite).unwrap();
        assert!((current.stage.scene.node(id).unwrap().props.x - 2.7).abs() < 1e-9);
        let timer = field(&current, loaded, "timer");
        assert_eq!(
            current
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: timer,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(1)
        );
        while !current.adopt_checkpoint_resources(&session, 1).unwrap() {}
        advance_clock(&mut current, &session, 0.5, 64);
        current.sync_stage(&session).unwrap();
        assert!(
            (current.stage.scene.node(id).unwrap().props.x - 4.2875).abs() < 1e-9,
            "restored x={}",
            current.stage.scene.node(id).unwrap().props.x
        );
        assert_eq!(
            current
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: timer,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(1)
        );
        advance_clock(&mut current, &session, 1., 64);
        assert_eq!(
            current
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: timer,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(2)
        );
        session.set_skip_reading(true, true, false);
        advance_clock(&mut current, &session, 1., 64);
        current.sync_stage(&session).unwrap();
        assert_eq!(current.stage.scene.node(id).unwrap().props.x, 100.);
        let delay = field(&current, loaded, "waiting");
        assert_eq!(
            current.vm.heap().host_payload(delay, 1).unwrap().as_int(),
            Some(0)
        );
        advance_clock(&mut current, &session, 7., 64);
        assert_eq!(
            current
                .vm
                .heap()
                .host_payload(field(&current, loaded, "waiting"), 1)
                .unwrap()
                .as_int()
                .unwrap(),
            3
        );
        registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(token).unwrap();
    }
}

#[test]
fn inactive_explicit_start_and_terminal_activity_keep_public_controls_source_asb() {
    let source = r#"
        use std::resume::{state,root};use reel::types::{ImageNode,Activity,Clock};
        class Scene {let mut sprite:ImageNode|null=null;let mut prepared:Activity|null=null;let mut done:Activity|null=null;let mut timer:Clock|null=null;}
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn inspect_loaded(loaded){
            if loaded!=null {
                if await loaded.done!="Completed" {throw "terminal result";}
                stage.replace(loaded.sprite);resume(loaded.prepared);resume(loaded.timer);
                narrator("resumed");
            }
        }
        fn opening(){
            let s=main();s.sprite=image("sprite.png",x:0,width:100,height:50);
            s.prepared=run(tween(s.sprite,duration:2,from:{x:10},x:50),paused:true,owner:s.sprite);
            s.done=run(delay(1),owner:s.sprite);s.timer=clock(interval:0.5,paused:true,owner:s.sprite);
            stage.replace(s.sprite);
            let loaded=persistent.write("capture",s);
            inspect_loaded(loaded);
        }
    "#;
    for encoded in [false, true] {
        let (session, mut old, _, value) = pending_resource_write(source, encoded);
        advance_clock(&mut old, &session, 2., 64);
        let old_registry = registry(&mut old);
        let owner = old.root_owner;
        let adapter =
            stage::resources::SceneCapture::new(&old.stage, &old.activities, old.read_limit);
        let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
        while !capture
            .step(old.vm.heap(), &old_registry, &adapter, 1, 1)
            .unwrap()
            .0
        {}
        let (document, charge) = capture.finish(old.vm.heap_mut()).unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
        drop(charge);
        let (mut session, mut current, ticket, _) = pending_resource_write(source, encoded);
        current.activities.revoke(current.vm.heap_mut()).unwrap();
        current.activities = activity::Activities::default();
        let registry = registry(&mut current);
        let owner = current.root_owner;
        let adapter = stage::resources::SceneRestore::new(
            &mut current.stage,
            &mut current.activities,
            owner,
            current.read_limit,
        );
        let mut restore = TypedRestore::new(current.vm.heap_mut(), owner, document).unwrap();
        while !restore
            .step(current.vm.heap_mut(), &registry, &adapter, 1, 1)
            .unwrap()
        {}
        let root = restore.value_root(current.vm.heap_mut(), owner).unwrap();
        let loaded = current.vm.heap().root_value(root).unwrap();
        restore.release(current.vm.heap_mut()).unwrap();
        while !current.prepare_checkpoint_resources(1).unwrap() {}
        while !current.adopt_checkpoint_resources(&session, 1).unwrap() {}
        assert!(!current.activities.needs_time());
        current
            .vm
            .respond(ticket, HostResponse::Ready(loaded))
            .unwrap();
        current.issued = None;
        models::drive(&mut current, &mut session, true);
        let sprite = field(&current, loaded, "sprite");
        let id = current.stage.id(current.vm.heap(), sprite).unwrap();
        current.sync_stage(&session).unwrap();
        assert_eq!(current.stage.scene.node(id).unwrap().props.x, 10.);
        advance_clock(&mut current, &session, 1., 64);
        current.sync_stage(&session).unwrap();
        assert_eq!(current.stage.scene.node(id).unwrap().props.x, 30.);
        let timer = field(&current, loaded, "timer");
        assert_eq!(
            current
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: timer,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(2)
        );
        registry.release(current.vm.heap_mut()).unwrap();
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn durable_derived_property_reports_its_path_and_capture_cancel_releases_root() {
    let source = SOURCE.replace("x:reference", "x:derive(||s.count)");
    let (_, mut old, _, value) = pending_resource_write(&source, false);
    let old_registry = registry(&mut old);
    let owner = old.root_owner;
    let baseline = old.vm.heap().stats().roots;
    let adapter = stage::resources::SceneCapture::new(&old.stage, &old.activities, old.read_limit);
    let mut capture = TypedCapture::new(old.vm.heap_mut(), owner, value).unwrap();
    let error = loop {
        match capture.step(old.vm.heap(), &old_registry, &adapter, 1, 1) {
            Ok((false, _)) => {}
            Ok((true, _)) => panic!("derived producer must be rebuilt"),
            Err(error) => break error,
        }
    };
    assert!(error.to_string().contains("node.x"), "{error}");
    assert!(
        error.to_string().contains("current view factory"),
        "{error}"
    );
    capture.cancel(old.vm.heap_mut()).unwrap();
    assert_eq!(old.vm.heap().stats().roots, baseline);
    assert_eq!(field(&old, value, "count").as_int(), Some(7));
    old_registry.release(old.vm.heap_mut()).unwrap();
}

fn capture_document(vm: &mut SessionVm, value: Value) -> Arc<TypedDocument> {
    capture_document_with_owner(vm, value, Value::NULL)
}
fn capture_document_with_owner(
    vm: &mut SessionVm,
    value: Value,
    default_owner: Value,
) -> Arc<TypedDocument> {
    let registry = registry(vm);
    let owner = vm.root_owner;
    let references = stage::resources::SceneCapture::new(&vm.stage, &vm.activities, vm.read_limit)
        .with_default_owner(default_owner);
    let mut capture = TypedCapture::new(vm.vm.heap_mut(), owner, value).unwrap();
    loop {
        let (done, used) = capture
            .step(vm.vm.heap(), &registry, &references, 1, 1)
            .unwrap();
        assert!(used <= 1);
        if done {
            break;
        }
    }
    let (document, charge) = capture.finish(vm.vm.heap_mut()).unwrap();
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    let document = Arc::new(TypedDocument::decode(&bytes).unwrap());
    drop(charge);
    registry.release(vm.vm.heap_mut()).unwrap();
    document
}
fn restore_document(
    vm: &mut SessionVm,
    document: Arc<TypedDocument>,
) -> (aside_vm::RootToken, Value) {
    vm.activities.revoke(vm.vm.heap_mut()).unwrap();
    vm.activities = activity::Activities::default();
    let registry = registry(vm);
    let owner = vm.root_owner;
    let references = stage::resources::SceneRestore::new(
        &mut vm.stage,
        &mut vm.activities,
        owner,
        vm.read_limit,
    );
    let mut restore = TypedRestore::new(vm.vm.heap_mut(), owner, document).unwrap();
    while !restore
        .step(vm.vm.heap_mut(), &registry, &references, 1, 1)
        .unwrap()
    {}
    let root = restore.value_root(vm.vm.heap_mut(), owner).unwrap();
    let value = vm.vm.heap().root_value(root).unwrap();
    restore.release(vm.vm.heap_mut()).unwrap();
    registry.release(vm.vm.heap_mut()).unwrap();
    while !vm.prepare_checkpoint_resources(1).unwrap() {}
    (root, value)
}
fn mount(vm: &mut SessionVm, session: &Session, node: Value) -> reel_ui::stage::NodeId {
    vm.stage
        .dispatch(
            &mut vm.vm,
            vm.root_owner,
            "reel::stage::replace",
            &[node],
            vm.read_limit,
            None,
        )
        .unwrap();
    vm.sync_stage(session).unwrap();
    vm.stage.id(vm.vm.heap(), node).unwrap()
}

#[test]
fn composed_occurrences_repeat_iteration_and_shared_curve_resume_source_asb() {
    let source = r#"
        use std::resume::{state,root}; use std::math::curve;
        use reel::types::{ImageNode,Activity};
        class Scene {
            let mut sprite:ImageNode|null=null;let mut motion:Activity|null=null;
            let mut curves:any=null;let mut descriptions:any=null;
        }
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn opening(){
            let s=main();s.sprite=image("sprite.png",x:0,y:0,width:100,height:50);
            let ramp=curve([{time:0,value:0,mode:"cubic"},{time:1,value:1}]);
            let rise=tween(s.sprite,duration:2,easing:ramp,from:{x:0},x:20);
            let fall=tween(s.sprite,duration:1,x:0);
            s.curves=[ramp,ramp];s.descriptions=[rise,rise];
            s.motion=run(parallel(sequence(rise,fall,rise,fall).repeat(times:2),
                sequence(delay(1),tween(s.sprite,duration:11,y:110))),owner:s.sprite);
            stage.replace(s.sprite);persistent.write("capture",s);
        }
    "#;
    for encoded in [false, true] {
        let (session, mut old, _, value) = pending_resource_write(source, encoded);
        advance_clock(&mut old, &session, 7.5, 1);
        let document = capture_document(&mut old, value);
        let (session, mut current, _, _) = pending_resource_write(source, encoded);
        let (root, loaded) = restore_document(&mut current, document);
        assert!(!current.activities.needs_time());
        let curves = field(&current, loaded, "curves");
        let curve = current.vm.heap().sequence_get(curves, 0).unwrap();
        assert!(curve.same_bits(current.vm.heap().sequence_get(curves, 1).unwrap()));
        assert_eq!(
            aside_std::sample_curve(current.vm.heap(), curve, 0.25).unwrap(),
            0.15625
        );
        let descriptions = field(&current, loaded, "descriptions");
        assert!(
            current
                .vm
                .heap()
                .sequence_get(descriptions, 0)
                .unwrap()
                .same_bits(current.vm.heap().sequence_get(descriptions, 1).unwrap())
        );
        let sprite = field(&current, loaded, "sprite");
        let id = mount(&mut current, &session, sprite);
        let props = &current.stage.scene.node(id).unwrap().props;
        assert!((props.x - 16.875).abs() < 1e-9);
        assert!((props.y - 65.).abs() < 1e-9);
        // Candidate preparation consumes no story time.
        current.activities.observe_time(40.).unwrap();
        current.activities.advance(&mut current.vm, 64).unwrap();
        while !current.adopt_checkpoint_resources(&session, 1).unwrap() {}
        advance_clock(&mut current, &session, 41., 1);
        current.sync_stage(&session).unwrap();
        let props = &current.stage.scene.node(id).unwrap().props;
        assert!((props.x - 10.).abs() < 1e-9);
        assert!((props.y - 75.).abs() < 1e-9);
        advance_clock(&mut current, &session, 44.6, 1);
        current.sync_stage(&session).unwrap();
        assert_eq!(current.stage.scene.node(id).unwrap().props.x, 0.);
        assert_eq!(current.stage.scene.node(id).unwrap().props.y, 110.);
        let motion = field(&current, loaded, "motion");
        assert_eq!(
            current.vm.heap().host_payload(motion, 1).unwrap().as_int(),
            Some(3)
        );
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn ordinary_node_schemas_bindings_and_child_edges_restore_source_asb() {
    let source = r#"
        use std::resume::{state,root};use reel::types::Node;
        class Scene {
            let mut amount:int=13;let mut selected:bool=true;let mut selection:string="a";
            let mut nodes:array=[];let mut group:Node|null=null;
        }
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn opening(){
            let s=main();
            s.nodes=[text("caption"),text_input(value:"line"),text_area(value:"many\nlines"),
                button("ok"),slider(value:bind(s.amount),min:0,max:100,
                    track:stack(),thumb:stack(height:10),thumb_width:18),
                toggle(label:"choice",value:bind(s.selected)),checkbox(label:"choice",value:bind(s.selected)),
                radio("a",value:bind(s.selection)),input(),image("sprite.png"),
                camera(camera_x:3,zoom:2,camera_z:25,focal_length:500) {
                    camera_plane(distance:650) { stack(width:10,height:10); };
                },scroll(scroll_y:5),row(wrap:true),column(gap:4),
                stack(),cursor(),grid(columns:3),popup(offset:6)];
            s.group=stack(children:s.nodes);stage.replace(s.group);persistent.write("capture",s);
        }
    "#;
    for encoded in [false, true] {
        let (_, mut old, _, value) = pending_resource_write(source, encoded);
        let document = capture_document(&mut old, value);
        let (session, mut current, _, _) = pending_resource_write(source, encoded);
        let (root, loaded) = restore_document(&mut current, document);
        let group = field(&current, loaded, "group");
        let id = mount(&mut current, &session, group);
        let nodes = field(&current, loaded, "nodes");
        assert_eq!(current.stage.scene.node(id).unwrap().children.len(), 18);
        for index in 0..18 {
            let child = current.vm.heap().sequence_get(nodes, index).unwrap();
            assert!(
                current
                    .vm
                    .heap()
                    .host_payload(child, 2)
                    .unwrap()
                    .same_bits(group)
            );
            let child_id = current.stage.id(current.vm.heap(), child).unwrap();
            assert_eq!(
                current.stage.scene.node(id).unwrap().children[index as usize],
                child_id
            );
        }
        let camera = current.vm.heap().sequence_get(nodes, 10).unwrap();
        let camera_id = current.stage.id(current.vm.heap(), camera).unwrap();
        let camera = current.stage.scene.node(camera_id).unwrap();
        assert!(matches!(&camera.kind,
            reel_ui::stage::NodeKind::Camera(view)
                if view.x==3. && view.zoom==2. && view.z==25. && view.focal_length==500.));
        assert!(
            matches!(&current.stage.scene.node(camera.children[0]).unwrap().kind,
            reel_ui::stage::NodeKind::CameraPlane { distance } if *distance == 650.)
        );
        let slider = current.vm.heap().sequence_get(nodes, 4).unwrap();
        let slider_id = current.stage.id(current.vm.heap(), slider).unwrap();
        let slider_node = current.stage.scene.node(slider_id).unwrap();
        assert!(
            matches!(slider_node.kind, reel_ui::stage::NodeKind::Slider { art, .. }
            if art.track == Some(slider_node.children[0]) && art.thumb == Some(slider_node.children[1])
                && art.thumb_width == 18.)
        );
        current
            .vm
            .heap_mut()
            .write_host_property(
                aside_vm::PropertyId {
                    host: slider,
                    field: 0,
                },
                Value::int(27),
                true,
            )
            .unwrap();
        assert_eq!(field(&current, loaded, "amount").as_int(), Some(27));
        let toggle = current.vm.heap().sequence_get(nodes, 5).unwrap();
        current
            .vm
            .heap_mut()
            .write_host_property(
                aside_vm::PropertyId {
                    host: toggle,
                    field: 1,
                },
                Value::bool(false),
                true,
            )
            .unwrap();
        assert_eq!(field(&current, loaded, "selected").as_bool(), Some(false));
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn curve_compiled_overflow_and_repeated_key_order_survive_data_restore() {
    let source = r#"
        use std::resume::{state,root};use std::math::curve;
        class Scene {let mut curves:array=[];}
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn opening(){let s=main();s.curves=[
            curve([{time:0,value:-1e308,mode:"cubic"},{time:1,value:1e308}]),
            curve([{time:-1,value:0,mode:"step"},{time:0,value:1},{time:0,value:2},{time:1,value:4}])
        ];persistent.write("capture",s);}
    "#;
    let (_, mut old, _, value) = pending_resource_write(source, false);
    let document = capture_document(&mut old, value);
    let (_, mut current, _, _) = pending_resource_write(source, false);
    let (root, loaded) = restore_document(&mut current, document);
    let curves = field(&current, loaded, "curves");
    let overflow = current.vm.heap().sequence_get(curves, 0).unwrap();
    assert!(
        aside_std::sample_curve(current.vm.heap(), overflow, 0.5)
            .unwrap()
            .is_nan()
    );
    let repeated = current.vm.heap().sequence_get(curves, 1).unwrap();
    for (time, expected) in [(-2., 0.), (-0.5, 0.), (0., 2.), (0.5, 3.), (2., 4.)] {
        assert_eq!(
            aside_std::sample_curve(current.vm.heap(), repeated, time).unwrap(),
            expected
        );
    }
    current.vm.heap_mut().release_root(root).unwrap();
}

#[test]
fn default_root_scope_owner_reconnects_current_identity_at_adoption() {
    let source = r#"
        use std::resume::{state,root};use reel::types::{ImageNode,Activity};
        class Scene {let mut sprite:ImageNode|null=null;let mut motion:Activity|null=null;}
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn opening(){let s=main();scope(body: |owner| {
            s.sprite=image("sprite.png",x:0,width:100,height:50);
            s.motion=run(tween(s.sprite,duration:10,x:100));
            stage.replace(s.sprite);persistent.write("capture",s);
        });}
    "#;
    let (session, mut old, ticket, value) = pending_resource_write(source, false);
    let old_owner = old.default_activity_owner(ticket.task).unwrap();
    assert!(super::super::scopes::is_scope(old.vm.heap(), old_owner));
    let motion = field(&old, value, "motion");
    assert!(
        old.vm
            .heap()
            .host_payload(motion, 3)
            .unwrap()
            .same_bits(old_owner)
    );
    advance_clock(&mut old, &session, 3., 1);
    let document = capture_document_with_owner(&mut old, value, old_owner);
    let (session, mut current, ticket, _) = pending_resource_write(source, false);
    let current_owner = current.default_activity_owner(ticket.task).unwrap();
    assert!(!current_owner.same_bits(old_owner));
    let (root, loaded) = restore_document(&mut current, document);
    let motion = field(&current, loaded, "motion");
    assert_eq!(
        current.vm.heap().host_payload(motion, 3).unwrap().tag(),
        aside_vm::ValueTag::Null
    );
    current.activities.set_restored_default_owner(current_owner);
    while !current.adopt_checkpoint_resources(&session, 1).unwrap() {}
    assert!(
        current
            .vm
            .heap()
            .host_payload(motion, 3)
            .unwrap()
            .same_bits(current_owner)
    );
    let sprite = field(&current, loaded, "sprite");
    let id = mount(&mut current, &session, sprite);
    advance_clock(&mut current, &session, 0.5, 1);
    current.sync_stage(&session).unwrap();
    assert_eq!(current.stage.scene.node(id).unwrap().props.x, 35.);
    current.vm.heap_mut().release_root(root).unwrap();
}

#[test]
fn character_aliases_data_and_current_native_callable_restore_source_asb() {
    let source = r#"
        use std::resume::{state,root};use reel::types::Character;
        class Scene {let mut speaker:Character|null=null;let mut alias:any=null;}
        state(Scene,"scene");root(main,"game");fn main(s:Scene=model(Scene())){s}
        fn inspect_loaded(loaded){if loaded!=null{loaded.speaker("restored speaker");}}
        fn opening(){let s=main();s.speaker=character("Sayori",id:"sayori");s.alias=s.speaker;
            let loaded=persistent.write("capture",s);inspect_loaded(loaded);}
    "#;
    for encoded in [false, true] {
        let (_, mut old, _, value) = pending_resource_write(source, encoded);
        let document = capture_document(&mut old, value);
        let (mut session, mut current, ticket, _) = pending_resource_write(source, encoded);
        let (root, loaded) = restore_document(&mut current, document);
        let speaker = field(&current, loaded, "speaker");
        assert!(speaker.same_bits(field(&current, loaded, "alias")));
        assert!(current.vm.callable_has_parameters(speaker).unwrap());
        current
            .vm
            .respond(ticket, HostResponse::Ready(loaded))
            .unwrap();
        current.issued = None;
        models::drive(&mut current, &mut session, true);
        let Some(crate::InteractionView::Dialogue(dialogue)) = session.view().interaction else {
            panic!("expected restored character speech")
        };
        assert_eq!(dialogue.text(), "restored speaker");
        assert_eq!(dialogue.lines()[0].speaker_id(), Some("sayori"));
        assert_eq!(dialogue.speaker_name(), "Sayori");
        current.vm.heap_mut().release_root(root).unwrap();
    }
}

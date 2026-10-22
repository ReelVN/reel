use super::*;
use reel_ui::stage::{GridOptions, NodeKind};

#[test]
fn flow_layout_binding_configuration_and_restore_share_registered_schemas() {
    let code = r#"fn dynamic(value) { value }
    fn opening() {
        let state=model({columns:4,weight:1,gap:8});
        let body=text("A remaining-width paragraph",flex:bind(state.weight));
        let line=row(width:640,gap:12){ text("Lucy",width:120); body; };
        let cards=grid(columns:bind(state.columns),column_width:140,row_height:80,row_gap:bind(state.gap)) {
            for slot in 0..12 { button(f"{slot+1}"); }
        };
        stage.replace(column(gap:24){line;cards;});
        if line.with(wrap:true,justify:"center",line_gap:20)!=line { throw "flow identity"; }
        let mut errors=0;
        try {cards.with(columns:0,row_height:40);} catch error {errors+=1;}
        try {line.justify="wrong";} catch error {errors+=1;}
        try {body.flex=dynamic(null);} catch error {errors+=1;}
        if errors!=3 || cards.row_height!=80 || cards.columns!=4 {throw "layout validation";}
        narrator("before");
        cards.with(columns:3,column_width:160,row_gap:10);
        body.with(flex:2);
        if state.columns!=3 || state.weight!=2 || state.gap!=10 {throw "bound writes";}
        let spacing=tween(cards,row_gap:20,duration:1,from:{row_gap:10});
        let wrap_spacing=tween(line,line_gap:40,duration:1);
        narrator("after");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let grid = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::Grid(_)))
            .unwrap()
            .id;
        assert_eq!(vm.stage().node(grid).unwrap().children.len(), 12);
        (session, vm) = scopes::restored(&mut vm, &session);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        box_style::drive(&mut vm, &mut session);
        assert_eq!(
            vm.stage().node(grid).unwrap().kind,
            NodeKind::Grid(GridOptions {
                columns: 3,
                column_width: Some(160.),
                row_height: Some(80.),
                row_gap: Some(10.),
                ..Default::default()
            })
        );
        assert_eq!(vm.stage().nodes().find(|n|matches!(&n.kind,NodeKind::Text(t) if t.to_string()=="A remaining-width paragraph")).unwrap().props.flex,2.);
    }
}

#[test]
fn flow_layout_editor_types_describe_configuration_and_grid_children() {
    use aside_compiler::semantic::TypeHint;
    let api = source::api();
    assert_eq!(
        api.member("reel::types::GridNode", "columns")
            .unwrap()
            .symbol
            .ty,
        TypeHint::Int
    );
    assert_eq!(
        api.member("reel::types::FlowNode", "wrap")
            .unwrap()
            .symbol
            .ty,
        TypeHint::Bool
    );
    assert_eq!(
        api.member("reel::types::FlowNode", "justify")
            .unwrap()
            .symbol
            .ty,
        TypeHint::String
    );
    assert!(api.member("reel::types::GridNode", "with").is_some());
    for code in [
        "grid(columns:false)",
        "row(wrap:3)",
        "text(\"x\",flex:null)",
    ] {
        assert!(
            source::compile_story(
                &format!("fn opening(){{let _={code};}}"),
                "layout-type",
                1,
                "opening"
            )
            .is_err(),
            "{code}"
        );
    }
}

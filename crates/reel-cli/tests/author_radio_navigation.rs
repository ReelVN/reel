//! Radio keyboard semantics execute through real source/ASB sessions and accepted frames.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{bridge::PresentationBridge, core::*, stage::NodeKind};
use support::*;

fn drivers(source: &str) -> Vec<AsideSessionUi> {
    let checked = reel_cli::check(source, "radio.aside").unwrap();
    let asb = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
    [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&asb).unwrap(),
    ]
    .into_iter()
    .map(|story| {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        driver
    })
    .collect()
}

fn press(
    d: &mut AsideSessionUi,
    b: &mut PresentationBridge,
    named: keyboard::NamedKey,
    shift: bool,
) {
    for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
        d.handle_text(
            b,
            TextEvent::Keyboard(keyboard::KeyboardEvent {
                state,
                key: keyboard::Key::Named(named),
                code: keyboard::Code::Unidentified,
                location: keyboard::Location::Standard,
                modifiers: if shift {
                    keyboard::Modifiers::SHIFT
                } else {
                    keyboard::Modifiers::empty()
                },
                repeat: false,
                is_composing: false,
            }),
        )
        .unwrap();
    }
    settle(d);
}

fn text(d: &AsideSessionUi, prefix: &str) -> String {
    d.adapter()
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Text(text) if text.as_ref().starts_with(prefix) => Some(text.to_string()),
            _ => None,
        })
        .unwrap()
}

const SOURCE: &str = r#"
fn main() {
    let state = model({value:"c", other:"y", changes:0});
    let changed = |e| {
        if e.target.value != e.value {throw "uncommitted selection";}
        state.changes += 1;
    };
    let before = button("Before", width:120,height:30);
    let a=radio("a","A",value:bind(state.value),on_change:changed,width:120,height:30,y:40);
    let b=radio("b","B",value:bind(state.value),enabled:false,width:120,height:30,y:80);
    let c=radio("c","C",value:bind(state.value),on_change:changed,width:120,height:30,y:120,decorated:false);
    let hidden=radio("hidden",value:bind(state.value),visible:false,width:120,height:30,y:160);
    let x=radio("x","X",value:bind(state.other),on_change:changed,width:120,height:30,x:180);
    let y=radio("y","Y",value:bind(state.other),on_change:changed,width:120,height:30,x:180,y:40);
    let after=button("Set C",x:350,width:120,height:30,on_click:||{state.value="c";});
    let disable=button("Disable C",x:350,y:40,width:120,height:30,on_click:||{c.enabled=false;});
    let status=text(derive(|| f"state:{state.value}:{state.other}:{state.changes}"),y:340);
    let focus=text(derive(|| if before.focused {"focus:before"} else if a.focused {"focus:a"}
        else if c.focused {"focus:c"} else if x.focused {"focus:x"} else if y.focused {"focus:y"}
        else if after.focused {"focus:after"} else if disable.focused {"focus:disable"} else {"focus:none"}),y:380);
    stage.replace(stack(children:[before,a,x,b,c,hidden,y,after,disable,status,focus]));
    narrator("ready");
}
"#;

#[test]
fn radio_groups_have_one_tab_stop_and_direction_selects_without_confirmation() {
    use keyboard::NamedKey::*;
    for mut d in drivers(SOURCE) {
        let (mut b, root) = bridge(&d);
        present(&mut d, &mut b, root, 1);
        for (key, shift, expected) in [
            (Tab, false, "before"),
            (Tab, false, "c"),
            (Tab, false, "y"),
            (Tab, true, "c"),
        ] {
            press(&mut d, &mut b, key, shift);
            assert_eq!(text(&d, "focus:"), format!("focus:{expected}"));
        }
        press(&mut d, &mut b, ArrowRight, false);
        assert_eq!(text(&d, "state:"), "state:a:y:1");
        assert_eq!(text(&d, "focus:"), "focus:a");
        present(&mut d, &mut b, root, 2);
        gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::DpadUp);
        assert_eq!(text(&d, "state:"), "state:c:y:2");
        assert_eq!(text(&d, "focus:"), "focus:c");
        present(&mut d, &mut b, root, 3);
        press(&mut d, &mut b, Enter, false);
        assert_eq!(text(&d, "state:"), "state:c:y:2");
        click_at(&mut d, &mut b, 60., 55.);
        present(&mut d, &mut b, root, 4);
        click_at(&mut d, &mut b, 60., 55.);
        assert_eq!(text(&d, "state:"), "state:a:y:3");
        click_at(&mut d, &mut b, 400., 15.);
        assert_eq!(text(&d, "state:"), "state:c:y:3");
        assert_eq!(
            text(&d, "focus:"),
            "focus:after",
            "script writes do not steal focus"
        );
        present(&mut d, &mut b, root, 5);
        press(&mut d, &mut b, Tab, true);
        assert_eq!(text(&d, "focus:"), "focus:y");
        press(&mut d, &mut b, Tab, true);
        assert_eq!(text(&d, "focus:"), "focus:c");
        click_at(&mut d, &mut b, 400., 55.);
        present(&mut d, &mut b, root, 6);
        press(&mut d, &mut b, Tab, true);
        press(&mut d, &mut b, Tab, true);
        press(&mut d, &mut b, Tab, true);
        assert_eq!(
            text(&d, "focus:"),
            "focus:a",
            "disabled selection falls back without changing value"
        );
        assert_eq!(text(&d, "state:"), "state:c:y:3");
        press(&mut d, &mut b, ArrowDown, false);
        assert_eq!(text(&d, "state:"), "state:a:y:4");
    }
}

#[test]
fn radio_group_keyboard_is_owned_by_control_and_modal_members_are_isolated() {
    use keyboard::NamedKey::*;
    let source = r#"
    fn main(){
        let state=model({value:1,open:false,keys:0,changes:0});
        let changed=||{state.changes+=1;};
        let outer=input(on_key:||{state.keys+=1;},width:600,height:400){
            radio(0,"zero",value:bind(state.value),width:120,height:30,on_change:changed);
            radio(1,"one",value:bind(state.value),y:40,width:120,height:30,on_change:changed);
            button("open",x:200,width:120,height:30,on_click:||{state.open=true;});
        };
        let first=radio(2,"two",value:bind(state.value),width:120,height:30,on_change:changed);
        let last=radio(3,"three",value:bind(state.value),y:40,width:120,height:30,on_change:changed);
        let modal=stack(modal:true,visible:bind(state.open),width:600,height:400){first;last;};
        stage.replace(stack{outer;modal;text(derive(||f"state:{state.value}:{state.keys}:{state.changes}"),y:380);});
        narrator("ready");
    }"#;
    for mut d in drivers(source) {
        let (mut b, root) = bridge(&d);
        present(&mut d, &mut b, root, 1);
        click_at(&mut d, &mut b, 50., 55.);
        press(&mut d, &mut b, ArrowRight, false);
        assert_eq!(
            text(&d, "state:"),
            "state:0:0:1",
            "ancestor on_key cannot take the radio arrow"
        );
        present(&mut d, &mut b, root, 2);
        click_at(&mut d, &mut b, 250., 15.);
        present(&mut d, &mut b, root, 3);
        press(&mut d, &mut b, Tab, false);
        press(&mut d, &mut b, ArrowLeft, false);
        assert_eq!(
            text(&d, "state:"),
            "state:3:0:2",
            "group wrapping stays inside the modal"
        );
        present(&mut d, &mut b, root, 4);
        press(&mut d, &mut b, ArrowRight, false);
        assert_eq!(text(&d, "state:"), "state:2:0:3");
    }
}

#[test]
fn virtual_radio_tab_does_not_build_rows_and_direction_resumes_after_acceptance() {
    use keyboard::NamedKey::*;
    let source = r#"
    fn main(){
        let items=[];for i in 0..10000 {items.append(i);}
        let state=model({selected:0,changes:0});
        let rows=list(||items,key:|item|item,virtual:true,build:|row|
            radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let after=button("After",width:120,height:30,y:180);
        stage.replace(stack{scroll(width:220,height:120){rows;}after;
            text(derive(||f"state:{state.selected}:{state.changes}"),y:340);
            text(derive(||if after.focused {"focus:after"} else {"focus:list"}),y:380);
        });narrator("ready");
    }"#;
    for total in [1000, 10000] {
        for mut d in drivers(&source.replace("0..10000", &format!("0..{total}"))) {
            let (mut b, root) = bridge(&d);
            let mut frame = 0;
            let flush = |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..6 {
                    *frame += 1;
                    present(d, b, root, *frame);
                    settle(d);
                }
            };
            flush(&mut d, &mut b, &mut frame);
            let constructed = d.adapter().stage().next_identity().unwrap();
            let mounted = d.adapter().stage().nodes().count();
            assert!(mounted < 30);
            press(&mut d, &mut b, Tab, false);
            let start = std::time::Instant::now();
            press(&mut d, &mut b, Tab, false);
            let elapsed = start.elapsed();
            assert_eq!(text(&d, "focus:"), "focus:after");
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(
                d.adapter().stage().next_identity().unwrap(),
                constructed,
                "Tab must not call the row factory to search for an exit"
            );
            eprintln!(
                "{total} radio items: mounted={mounted}, Tab input and VM drain={elapsed:?}, added nodes=0"
            );
            press(&mut d, &mut b, Tab, true);
            assert_eq!(text(&d, "focus:"), "focus:list");
            // Reach the accepted mounted edge without accepting a new frame.
            let mut waiting = None;
            for _ in 0..30 {
                let before = text(&d, "state:");
                press(&mut d, &mut b, ArrowDown, false);
                if text(&d, "state:") == before {
                    assert_eq!(
                        text(&d, "state:"),
                        before,
                        "unpresented target must not select"
                    );
                    waiting = Some(before);
                    break;
                }
            }
            let waiting = waiting.unwrap_or_else(|| {
                panic!(
                    "navigation reaches a virtual gap: {} focus={:?}",
                    text(&d, "state:"),
                    b.root().focused_widget()
                )
            });
            let selected: usize = waiting.split(':').nth(1).unwrap().parse().unwrap();
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(
                text(&d, "state:"),
                format!("state:{}:{}", selected + 1, selected + 1)
            );
            for expected in selected + 2..=25 {
                press(&mut d, &mut b, ArrowDown, false);
                flush(&mut d, &mut b, &mut frame);
                assert_eq!(text(&d, "state:"), format!("state:{expected}:{expected}"));
            }
            assert!(d.adapter().stage().nodes().count() < 40);
            let mut waiting = None;
            for _ in 0..30 {
                let before = text(&d, "state:");
                press(&mut d, &mut b, ArrowDown, false);
                if text(&d, "state:") == before {
                    waiting = Some(before);
                    break;
                }
            }
            let waiting = waiting.expect("second deferred boundary");
            press(&mut d, &mut b, Tab, false);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(
                text(&d, "state:"),
                waiting,
                "Tab cancels the pending selection"
            );
            assert_eq!(text(&d, "focus:"), "focus:after");
        }
    }
}

#[test]
fn virtual_radio_wraps_at_logical_ends_without_building_intermediate_rows() {
    use keyboard::NamedKey::*;
    let source = r#"
    fn main(){
        let items=[];for i in 0..10000 {items.append(i);}
        let state=model({selected:0,changes:0,ready:false});
        let rows=list(||items,key:|item|item,virtual:true,build:|row|
            radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let jump=button("Last",width:120,height:30,y:180,on_click:||{
            await rows.scroll_to(9999,align:"end");state.selected=9999;state.ready=true;
        });
        stage.replace(stack{scroll(width:220,height:120){rows;}jump;
            text(derive(||f"state:{state.selected}:{state.changes}:{state.ready}"),y:340);
            text(derive(||if jump.focused {"focus:jump"} else {"focus:list"}),y:380);
        });narrator("ready");
    }"#;
    for mut d in drivers(source) {
        let (mut b, root) = bridge(&d);
        let mut frame = 0;
        let flush = |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
            for _ in 0..12 {
                *frame += 1;
                present(d, b, root, *frame);
                settle(d);
            }
        };
        flush(&mut d, &mut b, &mut frame);
        let initial = d.adapter().stage().next_identity().unwrap();
        click_at(&mut d, &mut b, 60., 195.);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:9999:0:true");
        press(&mut d, &mut b, Tab, false);
        assert_eq!(text(&d, "focus:"), "focus:list");
        press(&mut d, &mut b, ArrowDown, false);
        assert_eq!(
            text(&d, "state:"),
            "state:9999:0:true",
            "wrap waits for the opposite accepted window"
        );
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:0:1:true");
        press(&mut d, &mut b, ArrowUp, false);
        assert_eq!(text(&d, "state:"), "state:0:1:true");
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:9999:2:true");
        assert!(
            d.adapter().stage().next_identity().unwrap().0 - initial.0 < 80,
            "only endpoint windows are constructed"
        );
        assert!(d.adapter().stage().nodes().count() < 40);
        press(&mut d, &mut b, ArrowDown, false);
        press(&mut d, &mut b, Tab, false);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(
            text(&d, "state:"),
            "state:9999:2:true",
            "leaving cancels an opposite-end request"
        );
        assert_eq!(text(&d, "focus:"), "focus:jump");
    }
}

#[test]
fn radio_group_crosses_distinct_virtual_lists_at_their_logical_ends() {
    use keyboard::NamedKey::*;
    let source = r#"
    fn main(){
        let left_items=[];let right_items=[];
        for i in 0..1000 {left_items.append(i);right_items.append(i+1000);}
        let state=model({selected:0,changes:0});
        let make=|row|radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
            on_change:||{state.changes+=1;});
        let left=list(||left_items,key:|item|item,virtual:true,build:make);
        let right=list(||right_items,key:|item|item,virtual:true,build:make);
        let prepare=button("Prepare",width:120,height:30,y:180,on_click:||{
            await right.scroll_to(1500);await left.scroll_to(999,align:"end");state.selected=999;
        });
        let last=button("Last",width:120,height:30,y:220,on_click:||{
            await right.scroll_to(1999,align:"end");state.selected=1999;
        });
        stage.replace(stack{
            scroll(width:220,height:120){left;}
            scroll(x:250,width:220,height:120){right;}
            prepare;last;text(derive(||f"state:{state.selected}:{state.changes}"),y:340);
        });narrator("ready");
    }"#;
    for mut d in drivers(source) {
        let (mut b, root) = bridge(&d);
        let mut frame = 0;
        let flush = |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
            for _ in 0..16 {
                *frame += 1;
                present(d, b, root, *frame);
                settle(d);
            }
        };
        flush(&mut d, &mut b, &mut frame);
        let initial = d.adapter().stage().next_identity().unwrap().0;
        click_at(&mut d, &mut b, 60., 195.);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:999:0");
        press(&mut d, &mut b, Tab, true);
        press(&mut d, &mut b, ArrowDown, false);
        assert_eq!(text(&d, "state:"), "state:999:0");
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(
            text(&d, "state:"),
            "state:1000:1",
            "enter the next list at its logical first row"
        );
        press(&mut d, &mut b, ArrowUp, false);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:999:2");
        click_at(&mut d, &mut b, 60., 235.);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:1999:2");
        press(&mut d, &mut b, Tab, false);
        press(&mut d, &mut b, ArrowDown, false);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:0:3", "wrap across both lists");
        press(&mut d, &mut b, ArrowUp, false);
        flush(&mut d, &mut b, &mut frame);
        assert_eq!(text(&d, "state:"), "state:1999:4");
        assert!(d.adapter().stage().next_identity().unwrap().0 - initial < 150);
        assert!(d.adapter().stage().nodes().count() < 60);
    }
}

#[test]
fn radio_discovers_its_group_beyond_another_virtual_lists_mounted_rows() {
    use keyboard::NamedKey::*;
    let source = r#"
    fn main(){
        let state=model({selected:0,other:-1,changes:0});
        let right_items=[];for i in 0..40 {right_items.append(i);}
        let left=list(||[0],key:|item|item,virtual:true,build:|row|
            radio(0,"left",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let right=list(||right_items,key:|item|item,virtual:true,build:|row|{
            if row.value==32 {
                radio(32,"right",value:bind(state.selected),width:180,height:40,
                    on_change:||{state.changes+=1;})
            }else if row.value%2==0 {
                text("header",width:180,height:40)
            }else{
                radio(row.value,"another group",value:bind(state.other),width:180,height:40)
            }
        });
        stage.replace(stack{
            scroll(width:220,height:120){left;}
            scroll(x:260,width:220,height:120){right;}
            text(derive(||f"state:{state.selected}:{state.other}:{state.changes}"),y:340);
        });narrator("ready");
    }"#;
    for mut d in drivers(source) {
        let (mut b, root) = bridge(&d);
        let mut frame = 0;
        for _ in 0..6 {
            frame += 1;
            present(&mut d, &mut b, root, frame);
            settle(&mut d);
        }
        assert!(
            !d.adapter().stage().nodes().any(|node| matches!(&node.kind,
            NodeKind::Radio{label:Some(label),..} if label=="right")),
            "the target starts unconstructed"
        );
        press(&mut d, &mut b, Tab, false);
        press(&mut d, &mut b, ArrowDown, false);
        for _ in 0..120 {
            frame += 1;
            present(&mut d, &mut b, root, frame);
            settle(&mut d);
            if text(&d, "state:") == "state:32:-1:1" {
                break;
            }
        }
        assert_eq!(
            text(&d, "state:"),
            "state:32:-1:1",
            "navigation discovers the group before wrapping to the left list"
        );
        assert!(
            d.adapter().stage().nodes().count() < 45,
            "only the current windows and pinned anchor stay mounted"
        );
        for (expected, changes) in [(0, 2), (32, 3)] {
            press(&mut d, &mut b, ArrowUp, false);
            for _ in 0..120 {
                frame += 1;
                present(&mut d, &mut b, root, frame);
                settle(&mut d);
                if text(&d, "state:") == format!("state:{expected}:-1:{changes}") {
                    break;
                }
            }
            assert_eq!(
                text(&d, "state:"),
                format!("state:{expected}:-1:{changes}"),
                "reverse traversal and wrapping use logical endpoints"
            );
        }
    }
}

use super::*;

#[test]
fn component_parts_keep_types_members_and_original_definitions_across_modules() {
    let (_project, root) = project_sources();
    let factory = r#"pub class Label {fn text(self)->string{"Title"} static fn zero()->int{0}}
pub fn parts()->table { {body:Label(), names:["one","two"]} }
pub fn inferred(flag:bool){if flag{return 7;}11}
pub fn labels(){["one","two"]}
pub fn heading(){"Ready"}
"#;
    std::fs::write(root.join("ui.aside"), factory).unwrap();
    let source = r#"use game::ui::{parts,inferred,labels,heading,Label};
fn main(){let panel=parts();let title=panel.body.text();let name=panel.names[0];let count=inferred(true);let names=labels();names.len();let caption=heading();caption.byte_len();Label.zero();title;name;count;panel.body. }
"#;
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    for (needle, expected) in [
        ("title;", "title: string"),
        ("name;", "name: string"),
        ("count;", "count: int"),
        ("inferred(true)", "-> int"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&main, position(source, needle, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(expected), "{needle}: {hover}");
    }
    let items = workspace
        .complete(
            &main,
            position(source, "panel.body.", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.label == "text" && item.kind == Some(CompletionItemKind::METHOD)),
        "{items:?}"
    );
    assert!(!items.iter().any(|item| item.label == "zero"));
    let items = workspace
        .complete(&main, position(source, "Label.", true), Encoding::Utf16)
        .unwrap();
    assert!(items.iter().any(|item| item.label == "zero"));
    assert!(!items.iter().any(|item| item.label == "text"));
    let items = workspace
        .complete(&main, position(source, "panel.", true), Encoding::Utf16)
        .unwrap();
    for name in ["get", "len", "has_own"] {
        assert!(
            items
                .iter()
                .any(|item| item.label == name && item.kind == Some(CompletionItemKind::METHOD)),
            "{items:?}"
        );
    }
    assert!(
        items
            .iter()
            .any(|item| item.label == "body" && item.kind == Some(CompletionItemKind::FIELD))
    );
    for (base, method) in [("names.", "append"), ("caption.", "byte_len")] {
        let items = workspace
            .complete(&main, position(source, base, true), Encoding::Utf16)
            .unwrap();
        assert!(
            items
                .iter()
                .any(|item| item.label == method && item.kind == Some(CompletionItemKind::METHOD)),
            "{items:?}"
        );
    }
    let body = workspace
        .definition(&main, position(source, "body.text", false), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(body.uri, file_uri(&root.join("ui.aside")).unwrap());
    assert_eq!(body.range.start, position(factory, "body:Label", false));
    let method = workspace
        .definition(&main, position(source, "text();", false), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(method.uri, body.uri);
    assert_eq!(method.range.start, position(factory, "text(self)", false));
    assert_eq!(
        workspace.analyses, 1,
        "queries share the same compiler facts"
    );
}

#[test]
fn record_completion_and_hover_follow_writes_and_remove_escaped_precision() {
    let source = r#"fn opaque(value:table){}
fn inspect(key:string){let parts={name:"first",count:1,len:4};let alias=parts;alias.name=12;
let updated=parts.name;parts[key]=false;let dynamic=parts.count;
let data={label:"value"};opaque(data);let escaped=data.label;updated;dynamic;escaped;parts. }
"#;
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    for (needle, expected) in [
        ("updated;", "updated: int"),
        ("dynamic;", "dynamic: any"),
        ("escaped;", "escaped: any"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&uri(), position(source, needle, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(expected), "{needle}: {hover}");
    }
    let items = workspace
        .complete(&uri(), position(source, "parts. ", true), Encoding::Utf16)
        .unwrap();
    for name in ["name", "count"] {
        assert!(
            items
                .iter()
                .any(|item| item.label == name && item.detail.as_deref() == Some("any")),
            "{items:?}"
        );
    }
    for name in ["get", "has_own"] {
        assert!(
            items
                .iter()
                .any(|item| item.label == name && item.kind == Some(CompletionItemKind::METHOD)),
            "{items:?}"
        );
    }
    let own_len = items
        .iter()
        .filter(|item| item.label == "len")
        .collect::<Vec<_>>();
    assert_eq!(own_len.len(), 1);
    assert_eq!(own_len[0].kind, Some(CompletionItemKind::FIELD));
    assert_eq!(own_len[0].detail.as_deref(), Some("any"));
}

use super::*;

#[test]
fn source_prefixes_browse_real_files_and_one_directory_level_with_unsaved_children() {
    let (_project, root) = project_sources();
    std::fs::create_dir_all(root.join("ui/unrelated/deep")).unwrap();
    std::fs::write(root.join("ui/dialogue.aside"), "pub fn speak(){}").unwrap();
    std::fs::write(
        root.join("ui/unrelated/deep/never_read.aside"),
        "invalid source",
    )
    .unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let dialogue = file_uri(&root.join("ui/dialogue.aside")).unwrap();
    let draft = file_uri(&root.join("ui/草稿.aside")).unwrap();
    let source = "use game::ui::{dialogue::{speak}}; fn main(){speak();}";
    let mut workspace = Workspace::new(project_options());
    workspace
        .open(draft.clone(), 1, "pub let unsaved=1;".into())
        .unwrap();
    workspace.open(main.clone(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&main, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    let disk_reads = workspace.disk_reads;
    for (needle, path) in [
        ("game::", "game"),
        ("ui::", "game::ui"),
        ("dialogue::", "game::ui::dialogue"),
    ] {
        let hover = workspace
            .hover(&main, position(source, needle, false), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert!(hover_text(hover).contains(&format!("use {path}")));
    }
    assert_eq!(
        workspace
            .definition(
                &main,
                position(source, "dialogue::", false),
                Encoding::Utf16
            )
            .unwrap()
            .unwrap()
            .uri,
        dialogue
    );
    let index = workspace
        .definition(&main, position(source, "ui::", false), Encoding::Utf16)
        .unwrap()
        .unwrap();
    let document = workspace.native_document(&index.uri).unwrap();
    assert!(document.text.contains("use game::ui::dialogue;"));
    assert!(document.text.contains("use game::ui::草稿;"));
    assert!(document.text.contains("use game::ui::unrelated;"));
    assert!(!document.text.contains("never_read"));
    workspace
        .open(index.uri.clone(), 1, document.text.to_string())
        .unwrap();
    assert!(
        workspace
            .diagnostics(&index.uri, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty()),
        "a directory index is a readonly view, not an executable import"
    );
    for (needle, expected) in [("dialogue;", dialogue), ("草稿;", draft)] {
        assert_eq!(
            workspace
                .definition(
                    &index.uri,
                    position(&document.text, needle, false),
                    Encoding::Utf16
                )
                .unwrap()
                .unwrap()
                .uri,
            expected
        );
    }
    assert_eq!(
        workspace.disk_reads, disk_reads,
        "directory browsing does not parse unrelated or already cached source files"
    );
    let source = "use game::missing::get;";
    workspace.open(main.clone(), 2, source.into()).unwrap();
    assert!(
        workspace
            .hover(&main, position(source, "missing::", false), Encoding::Utf16)
            .unwrap()
            .is_none()
    );
    assert!(
        workspace
            .definition(&main, position(source, "missing::", false), Encoding::Utf16)
            .unwrap()
            .is_none()
    );
}

#[test]
fn project_imports_use_one_module_for_short_and_qualified_paths() {
    let (_project, root) = project_sources();
    std::fs::create_dir(root.join("ui")).unwrap();
    let file = root.join("ui/dialogue.aside");
    std::fs::write(&file, "pub fn greet() {}").unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let declaration = file_uri(&file).unwrap();
    let source = "use ui::dialogue::{greet}; use game::ui::dialogue as panel; fn main(){greet(); panel.greet();}";
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&main, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|document| document.diagnostics.is_empty())
    );
    assert_eq!(workspace.snapshot(&main).unwrap().sources.len(), 2);
    let definitions = workspace
        .references(
            &main,
            position(source, "greet};", false),
            Encoding::Utf16,
            true,
        )
        .unwrap();
    assert_eq!(definitions.len(), 4);
    assert!(
        definitions
            .iter()
            .any(|location| location.uri == declaration)
    );
    let items = workspace
        .complete(
            &main,
            position(source, "ui::dialogue::{", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(items.iter().any(|item| item.label == "greet"));
}

#[test]
fn blank_and_nested_group_import_completion_discovers_source_exports() {
    let (_project, root) = project_sources();
    std::fs::create_dir_all(root.join("ui")).unwrap();
    let dialogue = root.join("ui/dialogue.aside");
    std::fs::write(
        &dialogue,
        "pub fn speak(){} pub class Actor{} let private=1;",
    )
    .unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    for (source, expected) in [
        ("use ", vec!["game"]),
        ("use game :: ui :: ", vec!["dialogue"]),
        ("use ui::dialogue::{ ", vec!["speak", "Actor"]),
        ("use game::ui::dialogue::{ ", vec!["speak", "Actor"]),
        ("use game::{ui::{dialogue::{ ", vec!["speak", "Actor"]),
        ("use ui::{dialogue::", vec!["speak", "Actor"]),
        ("use game::{ui::{dialogue:: ", vec!["speak", "Actor"]),
        (
            "use game::ui::dialogue::{/* members */ ",
            vec!["speak", "Actor"],
        ),
    ] {
        workspace.open(main.clone(), 1, source.into()).unwrap();
        let at = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&main, at, Encoding::Utf16).unwrap();
        for name in expected {
            assert!(
                items.iter().any(|item| item.label == name),
                "{source:?} missing {name}: {items:?}"
            );
        }
        assert!(items.iter().all(|item| item.label != "private"));
    }

    let source = "use ui::{dialogue::";
    workspace
        .open(
            file_uri(&dialogue).unwrap(),
            1,
            "pub fn from_unsaved_overlay() {}".into(),
        )
        .unwrap();
    workspace.open(main.clone(), 2, source.into()).unwrap();
    let at = position(source, "dialogue::", true);
    let reads = workspace.disk_reads;
    let items = workspace.complete(&main, at, Encoding::Utf16).unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.label == "from_unsaved_overlay")
    );
    assert!(items.iter().all(|item| item.label != "speak"));
    assert_eq!(workspace.disk_reads, reads);
    let analyses = workspace.analyses;
    workspace.complete(&main, at, Encoding::Utf16).unwrap();
    assert_eq!(workspace.analyses, analyses);
}

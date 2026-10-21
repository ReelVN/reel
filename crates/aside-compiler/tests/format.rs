use aside_compiler::{FormatError, Parse, SyntaxKind as K, format, format_parsed, parse};
use aside_syntax::WalkEvent;

fn shape(parsed: &Parse) -> Vec<String> {
    parsed
        .syntax()
        .preorder_with_tokens()
        .filter_map(|event| match event {
            WalkEvent::Enter(aside_compiler::SyntaxElement::Node(node)) => {
                Some(format!("({:?}", node.kind()))
            }
            WalkEvent::Leave(aside_compiler::SyntaxElement::Node(_)) => Some(")".into()),
            WalkEvent::Enter(aside_compiler::SyntaxElement::Token(token))
                if !token.kind().is_trivia() =>
            {
                Some(format!("{:?} {}", token.kind(), token.text()))
            }
            _ => None,
        })
        .collect()
}

fn checked(source: &str) -> String {
    let original = parse(source).unwrap();
    assert!(
        original.is_ok(),
        "original: {:?}\n{source}",
        original.diagnostics()
    );
    let formatted = format_parsed(&original).unwrap();
    let parsed = parse(&formatted).unwrap();
    assert!(
        parsed.is_ok(),
        "formatted: {:?}\n{formatted}",
        parsed.diagnostics()
    );
    assert_eq!(
        shape(&original),
        shape(&parsed),
        "changed CST\n{source}\n---\n{formatted}"
    );
    let comments = |parsed: &Parse| {
        parsed
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| matches!(token.kind(), K::LineComment | K::BlockComment))
            .map(|token| token.text().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(comments(&original), comments(&parsed));
    let docs = |parsed: &Parse| {
        parsed
            .syntax()
            .descendants()
            .filter(|node| matches!(node.kind(), K::FnDecl | K::ClassDecl | K::LetDecl))
            .map(|node| {
                aside_compiler::documentation::declaration(&node)
                    .map(|documentation| documentation.markdown)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        docs(&original),
        docs(&parsed),
        "changed declaration documentation\n{formatted}"
    );
    assert_eq!(
        format(&formatted).unwrap(),
        formatted,
        "not idempotent\n{formatted}"
    );
    formatted
}

#[test]
fn statements_children_and_short_expressions() {
    assert_eq!(
        checked(
            "fn room(){stack(width:960,height:540){text(\"Title\",font_size:32);if state.ready{button(\"Continue\",on_click:next);}}}"
        ),
        "fn room() {\n    stack(width: 960, height: 540) {\n        text(\"Title\", font_size: 32);\n        if state.ready {\n            button(\"Continue\", on_click: next);\n        }\n    }\n}\n"
    );
    assert_eq!(
        checked("let hint=text(\"Ready\",x:24,y:30);"),
        "let hint = text(\"Ready\", x: 24, y: 30);\n"
    );
    assert_eq!(checked(" \r\n\t"), "");
}

#[test]
fn calls_fill_short_arguments_and_chains_share_one_continuation_indent() {
    assert_eq!(
        checked(
            "let hint=text(\"Choose a word\",x:24,y:30,width:480,height:60,font_size:24,color:\"#ffffff\",text_align:\"center\",visible:bind(state.ready));"
        ),
        "let hint = text(\n    \"Choose a word\", x: 24, y: 30, width: 480, height: 60, font_size: 24, color: \"#ffffff\",\n    text_align: \"center\", visible: bind(state.ready)\n);\n"
    );
    let chain = checked(
        "let view = presentation.configure(width:960,height:540).background(\"images/the-sunlit-classroom.png\").portrait(\"Lucy\",position:\"center\").show();",
    );
    assert!(chain.contains("\n    .background("), "{chain}");
    assert!(chain.contains("\n    .portrait("), "{chain}");
    assert!(chain.contains("\n    .show();"), "{chain}");
    assert!(!chain.contains("\n        ."), "{chain}");
    let single = checked("fn main(){stage.replace(column(x:24,y:24,gap:24){controls;caption;});}");
    assert!(single.contains("    stage.replace("), "{single}");
    assert!(!single.contains("\n        .replace"), "{single}");
}

#[test]
fn deep_call_width_and_long_conditions_keep_field_access_together() {
    for length in 20..70 {
        let source = format!(
            "fn main(){{if ready{{stack(){{let value=call(first:\"{}\",second:42,third:true,fourth:50);}}}}}}",
            "x".repeat(length)
        );
        let output = checked(&source);
        assert!(
            output.lines().all(|line| line.chars().count() <= 100),
            "length {length}\n{output}"
        );
    }
    let output = checked(
        "fn main(){if self.ready.value && event.delta_y > 0 && event.x >= 105 && event.x < 1177 && event.y >= 115 && event.y < 675 && self.viewport.scroll_y + self.viewport.viewport_height >= self.viewport.content_height {go();}}",
    );
    assert!(output.contains("\n        && event.y"), "{output}");
    assert!(output.contains("self.viewport.viewport_height"), "{output}");
    assert!(!output.contains("\n        ."), "{output}");
    assert!(
        output.lines().all(|line| line.chars().count() <= 100),
        "{output}"
    );
}

#[test]
fn comments_docs_strings_and_numbers_keep_their_contents() {
    let source = "// Heading\r\n\r\n/// Say hello.\r\n/// Exact  spacing.\r\nfn say( value=0x00_FF ){ // inline\r\nlet text=f\"\"\"line 1\n  { value +  1 }\nline 3\"\"\";/* keep\nbody */let raw=r\"a\\b\";value;}";
    let formatted = checked(source);
    assert!(
        formatted.contains("/// Say hello.\n/// Exact  spacing.\nfn say(value = 0x00_FF)"),
        "{formatted}"
    );
    assert!(formatted.contains("f\"\"\"line 1\n  { value +  1 }\nline 3\"\"\""));
    assert!(formatted.contains("/* keep\nbody */"));
    assert_eq!(
        checked("// keep trailing spaces  \n"),
        "// keep trailing spaces  \n"
    );
    assert_eq!(
        checked("fn main(){run();// keep  \n}"),
        "fn main() {\n    run(); // keep  \n}\n"
    );
}

#[test]
fn postfix_after_children_and_statement_boundaries_keep_their_cst() {
    for source in [
        "stack(){text(\"A\");}.configure(width:960,height:540).very_long_operation(\"a long string that forces this expression over the line width\");",
        "stack(){}\n(next)();",
        "let view=stack(){text(\"A\");}\n.configure(width:960);",
        "await stack(){text(\"A\");}.show().wait();",
        "stack(){} + \"a long value that keeps the original children and operator attached to one expression\" + next;",
        "foo /* before call */ (value /* close */); node // member comment\n.method();",
        "fn main(){if ready{}else if other{}else{};try {run();}catch error {throw error;}finally {close();}}",
    ] {
        checked(source);
    }
}

#[test]
fn classes_patterns_types_closures_and_match_arms() {
    checked(
        "use game::{room,ui::{button as action,text}}; class Room:Base {let value:Int=1;pub fn show(...items){let callback=|value,other=2|value+other;match value {0=>null,1=>{run();},_=>value,}}} fn main(value:(Int,String))->Int|Null {let {first:item,...rest}=value;for (index,item) in items {item;}return null;}",
    );
}

#[test]
fn unicode_columns_and_long_spines_are_bounded_and_preserved() {
    let unicode = format!("let text=call(\"{}\",value:1);", "中".repeat(70));
    assert_eq!(checked(&unicode).lines().count(), 1);
    let chain = format!("let value=root{};", ".next()".repeat(2000));
    let output = checked(&chain);
    assert_eq!(output.lines().count(), 2001);
}

#[test]
fn invalid_source_produces_original_parse_diagnostics() {
    let source = "fn main( {";
    let parsed = parse(source).unwrap();
    let Err(FormatError::Syntax(diagnostics)) = format(source) else {
        panic!("invalid syntax was formatted")
    };
    assert_eq!(diagnostics, parsed.diagnostics());
    assert_eq!(format_parsed(&parsed).unwrap_err(), diagnostics);
}

fn corpus(root: &std::path::Path) -> usize {
    let started = std::time::Instant::now();
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "aside")
            {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    let mut bytes = 0usize;
    let mut largest = (std::path::PathBuf::new(), 0usize);
    let mut slowest = (std::path::PathBuf::new(), std::time::Duration::ZERO);
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        bytes += source.len();
        if source.len() > largest.1 {
            largest = (file.clone(), source.len());
        }
        let file_started = std::time::Instant::now();
        let result = std::panic::catch_unwind(|| checked(&source));
        assert!(result.is_ok(), "formatter corpus file: {}", file.display());
        let elapsed = file_started.elapsed();
        if elapsed > slowest.1 {
            slowest = (file.clone(), elapsed);
        }
    }
    eprintln!(
        "{}",
        serde_json::json!({
            "root": root, "files": files.len(), "source_bytes": bytes,
            "verification_ms": started.elapsed().as_secs_f64() * 1000.0,
            "largest_file": largest.0, "largest_file_bytes": largest.1,
            "slowest_file": slowest.0, "slowest_verification_ms": slowest.1.as_secs_f64() * 1000.0,
            "checks": ["non-trivia CST and token spelling", "comment spelling/order", "declaration documentation", "idempotence"],
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        })
    );
    files.len()
}

#[test]
fn public_examples_preserve_tokens_cst_and_are_idempotent() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let count = corpus(&root);
    assert!(
        count > 50,
        "public example corpus unexpectedly empty: {count}"
    );
    eprintln!("formatted {count} public example files without changing their source");
}

#[test]
#[ignore = "requires an explicitly selected local author project"]
fn external_author_corpus_preserves_tokens_cst_and_is_idempotent() {
    let root = std::env::var_os("ASIDE_FORMAT_CORPUS")
        .expect("ASIDE_FORMAT_CORPUS must name a source directory");
    let count = corpus(std::path::Path::new(&root));
    assert!(count > 0);
    eprintln!("formatted {count} author files without changing their source");
}

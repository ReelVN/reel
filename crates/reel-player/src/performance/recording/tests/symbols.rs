//! Offline symbol join for the exact ASB used by a sampled probe run.
use aside_vm::bytecode::{Pc, instruction_spec};
use std::{fs, io::Write as _, path::PathBuf};

#[test]
#[ignore = "offline VM profile symbol export; set REEL_PROFILE_ASB and REEL_PROFILE_SYMBOLS"]
fn dump_vm_profile_symbols() {
    let input = std::env::var_os("REEL_PROFILE_ASB").expect("REEL_PROFILE_ASB");
    let output =
        PathBuf::from(std::env::var_os("REEL_PROFILE_SYMBOLS").expect("REEL_PROFILE_SYMBOLS"));
    fs::create_dir_all(&output).unwrap();
    let story = crate::distribution::decode_story(&fs::read(input).unwrap()).unwrap();
    let p = story.program.package();
    let symbol =
        |id: aside_vm::bytecode::SymbolId| p.symbols[id.index()].replace(['\t', '\r', '\n'], " ");
    let mut functions = fs::File::create(output.join("functions.tsv")).unwrap();
    let mut instructions = fs::File::create(output.join("instructions.tsv")).unwrap();
    let mut natives = fs::File::create(output.join("natives.tsv")).unwrap();
    let mut symbols = fs::File::create(output.join("symbols.tsv")).unwrap();
    writeln!(symbols, "symbol\ttext").unwrap();
    for (id, name) in p.symbols.iter().enumerate() {
        writeln!(symbols, "{id}\t{}", name.replace(['\t', '\r', '\n'], " ")).unwrap();
    }
    writeln!(
        functions,
        "program\tfn\tmodule\tname\tkind\tbindings\tregisters"
    )
    .unwrap();
    writeln!(instructions, "fn\tpc\topcode\ta\tb\tc\tfile\tline\tsource").unwrap();
    writeln!(natives, "native\tsymbol").unwrap();
    for (id, native) in p.natives.iter().enumerate() {
        writeln!(natives, "{id}\t{}", symbol(native.symbol)).unwrap();
    }
    for (id, function) in p.functions.iter().enumerate() {
        writeln!(
            functions,
            "{}\t{id}\t{}\t{}\t{:?}\t{}\t{}",
            p.content_version,
            symbol(p.modules[function.module.index()].path),
            symbol(function.name),
            function.kind,
            function.bindings.len(),
            function.register_count
        )
        .unwrap();
        for entry in function.code.indexed_iter() {
            let (pc, instruction) = entry.unwrap();
            let (file, line, source) = function.source_map.source_at(Pc(pc as u32)).map_or_else(
                || (String::new(), 0, String::new()),
                |span| {
                    let file = &p.sources[span.file.index()];
                    let line = file.text[..span.start as usize]
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count()
                        + 1;
                    let source = file.text[span.start as usize..span.end as usize]
                        .replace(['\t', '\r', '\n'], " ");
                    (symbol(file.path), line, source)
                },
            );
            let [a, b, c] = instruction.operands;
            writeln!(
                instructions,
                "{id}\t{pc}\t{:?}\t{a}\t{b}\t{c}\t{file}\t{line}\t{source}",
                instruction_spec(instruction.opcode).unwrap().opcode
            )
            .unwrap();
        }
    }
}

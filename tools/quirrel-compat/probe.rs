//! Calls the real pinned Aside parser. No binding, lowering or execution.
use std::{env, fs, process};

fn main() {
    let mut failed = false;
    for path in env::args().skip(1) {
        let source = fs::read_to_string(&path).expect("read fixture");
        match aside_compiler::parse(&source) {
            Ok(parsed) => {
                let status = if parsed.is_ok() { "parse_ok" } else { "parse_failed" };
                println!("RESULT\t{path}\t{status}\t{}\t{}\t{}", source.len(),
                         parsed.diagnostics().len(), parsed.exhausted());
                for diagnostic in parsed.diagnostics() {
                    println!("DIAG\t{path}\t{}\t{}\t{}\t{:?}", diagnostic.code.as_str(),
                             u32::from(diagnostic.range.start()), u32::from(diagnostic.range.end()),
                             diagnostic.message);
                }
                failed |= !parsed.is_ok();
            }
            Err(error) => {
                println!("RESULT\t{path}\tsource_rejected\t{}\t1\tfalse", source.len());
                println!("DIAG\t{path}\tADMISSION\t0\t0\t{error:?}");
                failed = true;
            }
        }
    }
    if failed { process::exit(1); }
}

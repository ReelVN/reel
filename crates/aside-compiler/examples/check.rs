use aside_compiler::{EntryPoint, ParseOptions, parse_with_options};
use codespan_reporting::{
    files::SimpleFiles,
    term::{
        self, Config,
        termcolor::{ColorChoice, StandardStream},
    },
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: check FILE [module|block|expr|pattern|type]")?;
    let entry = match args.next().as_deref() {
        None | Some("module") => EntryPoint::Module,
        Some("block") => EntryPoint::Block,
        Some("expr") => EntryPoint::Expression,
        Some("pattern") => EntryPoint::Pattern,
        Some("type") => EntryPoint::Type,
        _ => return Err("unknown entry point".into()),
    };
    let source = std::fs::read_to_string(&path)?;
    let result = parse_with_options(
        &source,
        ParseOptions {
            entry,
            ..Default::default()
        },
    )?;
    let mut files = SimpleFiles::new();
    let id = files.add(path, source);
    let writer = StandardStream::stderr(ColorChoice::Auto);
    for d in result.diagnostics() {
        term::emit_to_write_style(
            &mut writer.lock(),
            &Config::default(),
            &files,
            &d.to_codespan(id),
        )?;
    }
    if std::env::var_os("ASIDE_DUMP_TREE").is_some() {
        println!("{:#?}", result.syntax());
    }
    if !result.is_ok() {
        std::process::exit(1);
    }
    Ok(())
}

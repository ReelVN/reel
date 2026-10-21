//! Reel's registered author environment over the Aside stdio language server.
use clap::Parser;

#[derive(Parser)]
#[command(
    version,
    about = "Aside language server with Reel's registered author API"
)]
struct Arguments {}

fn main() {
    Arguments::parse();
    if let Err(error) = aside_lsp::run_stdio(reel_cli::editor::options()) {
        eprintln!("reel-lsp: {error}");
        std::process::exit(1);
    }
}

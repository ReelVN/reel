fn main() {
    if let Err(error)=aside_lsp::run_stdio(aside_lsp::ServerOptions::default()) {
        eprintln!("aside-lsp: {error}");std::process::exit(1);
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src/platform/vita/startup.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("vita") {
        return;
    }
    cc::Build::new()
        .file("src/platform/vita/startup.c")
        .warnings(true)
        .compile("reel_vita_startup");
    // The packager discovers this configuration symbol after ELF linking.
    for symbol in ["sceUserMainThreadStackSize", "_newlib_heap_size_user"] {
        println!("cargo:rustc-link-arg-bin=reel-vita=-Wl,--undefined={symbol}");
    }
}

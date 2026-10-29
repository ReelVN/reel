fn main() {
    println!("cargo:rerun-if-changed=src/platform/vita/startup.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("vita") {
        return;
    }
    cc::Build::new()
        .file("src/platform/vita/startup.c")
        .warnings(true)
        .compile("reel_vita_startup");
    // Retain the main-thread metadata for packaging and the complete process
    // allocator family before CRT initialization.
    for symbol in [
        "sceUserMainThreadStackSize",
        "vita_native_process_heap_anchor",
    ] {
        println!("cargo:rustc-link-arg-bin=reel-vita=-Wl,--undefined={symbol}");
    }
}

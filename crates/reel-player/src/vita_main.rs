//! Native distribution entry; all package/resource interpretation remains shared.
#[cfg(target_os = "vita")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    reel_player::platform::vita::Runtime::run(|runtime| {
        runtime.run_launcher(std::path::Path::new("ux0:/data/reel"))
    })
}
#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("reel-vita requires the armv7-sony-vita-newlibeabihf target");
    std::process::exit(1);
}

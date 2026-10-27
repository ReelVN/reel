//! Device probe; desktop performance checks use the WGPU scenario host.
#[cfg(target_os = "vita")]
mod device;

fn main() {
    #[cfg(target_os = "vita")]
    device::main();
    #[cfg(not(target_os = "vita"))]
    {
        eprintln!("reel-gpu-probe requires the armv7-sony-vita-newlibeabihf target");
        std::process::exit(1);
    }
}

//! Hardware contract checks; performance collection runs through the player.
#[cfg(target_os = "vita")]
#[path = "mod.rs"]
mod device_checks;
#[cfg(any(target_os = "vita", test))]
mod options;

// Keep the production process allocator and native startup archive linked.
#[cfg(target_os = "vita")]
use reel_player as _;

fn main() {
    #[cfg(target_os = "vita")]
    device_checks::main();
    #[cfg(not(target_os = "vita"))]
    {
        eprintln!("reel-device-checks requires the armv7-sony-vita-newlibeabihf target");
        std::process::exit(1);
    }
}

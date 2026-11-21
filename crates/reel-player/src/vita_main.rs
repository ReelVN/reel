//! Native distribution entry; all package/resource interpretation remains shared.
#[cfg(target_os = "vita")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::Path;

    // Mode C enables the GPU performance configuration while retaining WLAN.
    vita_native::app::power::request_configuration(vita_native::app::power::ConfigurationMode::C)?;
    vita_native::app::power::request_arm_clock_frequency(444)?;
    #[cfg(feature = "performance")]
    {
        let path = Path::new("ux0:/data/reel/performance.json");
        let active = path.with_file_name("performance.active.json");
        if path.try_exists()? && active.try_exists()? {
            std::fs::remove_file(&active)?;
        }
        let request = match std::fs::rename(path, &active) {
            Ok(()) => Some(reel_player::performance::Request::read(&active)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(request) = request {
            let result = reel_player::platform::vita::Runtime::run(|runtime| {
                runtime.set_performance(request.clone())?;
                runtime.run_package(&request.package, &request.saves)
            });
            if let Err(error) = &result {
                request.write_status(
                    reel_player::performance::StatusState::Error,
                    Some(&error.to_string()),
                )?;
            }
            return result;
        }
    }
    let title_id = vita_native::app::title_id()?;
    let game = Path::new("ux0:/data/reel/games").join(title_id);
    let saves = game.join("saves");
    let package = game.join(reel_package::distribution::MAIN_PACKAGE);
    reel_player::platform::vita::Runtime::run(|runtime| runtime.run_package(&package, &saves))
}
#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("reel-vita requires the armv7-sony-vita-newlibeabihf target");
    std::process::exit(1);
}

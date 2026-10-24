fn main() -> Result<(), Box<dyn std::error::Error>> {
    reel_player::platform::desktop::run_session_host()
}

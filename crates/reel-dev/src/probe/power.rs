//! Explicit device power configuration for same-binary timing comparisons.
use super::{Result, directory, log};
use vita_native::app::power::{self, ConfigurationMode};

fn observe(phase: &str) {
    let clocks = power::clock_frequencies();
    log(format_args!(
        "POWER phase={phase} arm_mhz={:?} gpu_mhz={:?} bus_mhz={:?} gpu_xbar_mhz={:?}",
        clocks.arm_mhz, clocks.gpu_mhz, clocks.bus_mhz, clocks.gpu_xbar_mhz
    ));
}

pub(super) fn configure() -> Result {
    let value = match std::fs::read_to_string(directory().join("power-mode.txt")) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "inherit".into(),
        Err(error) => return Err(error.into()),
    };
    let mode = match value.trim() {
        "inherit" => None,
        "a" => Some(ConfigurationMode::A),
        "c" => Some(ConfigurationMode::C),
        value => {
            return Err(format!("unknown power mode {value}; expected inherit, a, or c").into());
        }
    };
    observe("before");
    if let Some(mode) = mode {
        let start = std::time::Instant::now();
        let result = power::request_configuration(mode);
        log(format_args!(
            "POWER request={mode:?} elapsed_us={} result={result:?}",
            start.elapsed().as_micros()
        ));
        observe("after");
        result?;
    } else {
        log(format_args!("POWER request=inherit"));
    }
    let result = power::request_arm_clock_frequency(444);
    log(format_args!("POWER arm_request_mhz=444 result={result:?}"));
    observe("after_arm_request");
    result?;
    Ok(())
}

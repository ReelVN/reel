//! Enumerate/negotiate/build/play the default CPAL output without window/render.
#[cfg(all(
    feature = "cpal-output",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
fn main() {
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    use reel_core::{clock::MonoTime, wake::WakeSignal};
    use reel_media::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let mut output = match CpalOutput::new(WakeSignal::new(|| {}), MonoTime(0)) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("device_unavailable: {error}");
            std::process::exit(2);
        }
    };
    println!("caps={:?}", output.capabilities());
    let format = output
        .configure(AudioRequest::default())
        .expect("negotiate known layout");
    println!("negotiated={format:?}");
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut kira_fill = mixer.take_fill().unwrap();
    if let Err(rejected) = output.start(Box::new(move |pcm| {
        observed.fetch_add(1, Ordering::Relaxed);
        kira_fill(pcm);
    })) {
        eprintln!("start_rejected_uncalled_fill: {:?}", rejected.reason);
        drop(rejected.request);
        drop(mixer);
        service
            .drain_and_join()
            .expect("final host drain after refusal");
        std::process::exit(3);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!(
        "status={:?} clock={:?}",
        output.status(),
        output.clock_observation()
    );
    println!("stop={:?}", output.stop());
    assert_eq!(output.status().state, AudioState::Stopped);
    assert_eq!(
        output.clock_observation().precision,
        ClockPrecision::Unavailable
    );
    let at_stop = calls.load(Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(calls.load(Ordering::Relaxed), at_stop);
    println!("fill_calls={at_stop} post_stop_fill_calls=0");
    drop(mixer);
    println!("final_reclamation={:?}", service.drain_and_join().unwrap());
}
#[cfg(not(all(
    feature = "cpal-output",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
)))]
fn main() {
    eprintln!("cpal-output is disabled or this target has no desktop adapter");
}

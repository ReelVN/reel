use super::*;
use crate::{
    hal::{Host, PlatformHost},
    platform::winit::WinitHost,
};
use std::{sync::Arc, thread::JoinHandle};
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::window::Window;

#[test]
#[ignore = "requires a native event loop and creates one hidden window"]
fn background_work_wakes_an_idle_window_without_redrawing() {
    const NOTIFICATIONS_PER_BATCH: usize = 1024;
    struct Harness {
        wake: WakeSignal,
        host: Option<WinitHost>,
        worker: Option<JoinHandle<()>>,
        deadline: Instant,
        batches: usize,
        native_wakes: usize,
        redraws: usize,
    }
    impl ApplicationHandler for Harness {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = event_loop
                .create_window(Window::default_attributes().with_visible(false))
                .unwrap();
            let mut host = WinitHost::new(Arc::new(window)).with_event_wake(self.wake.clone());
            host.wait_until(Some(self.deadline));
            host.apply(event_loop);
            self.host = Some(host);
        }
        fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
            self.native_wakes += 1;
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            if matches!(event, WindowEvent::RedrawRequested) {
                self.redraws += 1;
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            if self.native_wakes >= 2 || Instant::now() >= self.deadline {
                let host = self.host.as_mut().unwrap();
                host.request_exit();
                host.apply(event_loop);
            } else if self.worker.is_none() {
                self.wake.acknowledge();
                let notify = self.host.as_ref().unwrap().wake_callback();
                self.batches += 1;
                self.worker = Some(std::thread::spawn(move || {
                    for _ in 0..NOTIFICATIONS_PER_BATCH {
                        notify();
                    }
                }));
            }
        }
    }
    #[cfg(target_os = "linux")]
    let event_loop = EventLoop::builder()
        .with_x11()
        .with_any_thread(true)
        .build()
        .unwrap();
    #[cfg(target_os = "windows")]
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut harness = Harness {
        wake: event_wake(event_loop.create_proxy()),
        host: None,
        worker: None,
        deadline: Instant::now() + Duration::from_secs(10),
        batches: 0,
        native_wakes: 0,
        redraws: 0,
    };
    event_loop.run_app(&mut harness).unwrap();
    assert_eq!(harness.batches, 2);
    assert_eq!(harness.native_wakes, harness.batches, "one wake per batch");
    assert_eq!(harness.redraws, 0);
    assert!(
        Instant::now() < harness.deadline,
        "native exit waited for the previous deadline"
    );
}

//! Thread-bound UI requests, independent of retained background wake tokens.
use reel_ui::app::RenderRootSignal;
use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(super) struct Signals {
    redraw: Cell<bool>,
    pub(super) exit: Cell<bool>,
    pub(super) visible: Cell<bool>,
    pub(super) title: RefCell<String>,
    pub(super) text: super::vita_text::Signals,
}
impl Signals {
    pub(super) fn request_redraw(&self) {
        self.redraw.set(true);
    }
    pub(super) fn take_redraw(&self) -> bool {
        self.redraw.replace(false)
    }
    pub(super) fn has_redraw(&self) -> bool {
        self.redraw.get()
    }
    pub(super) fn receive(&self, signal: RenderRootSignal, wake: impl FnOnce()) {
        match signal {
            RenderRootSignal::RequestRedraw | RenderRootSignal::RequestAnimFrame => {
                self.request_redraw();
            }
            RenderRootSignal::Exit => {
                self.exit.set(true);
                wake();
            }
            RenderRootSignal::SetTitle(title) => {
                *self.title.borrow_mut() = title;
            }
            signal @ (RenderRootSignal::StartIme
            | RenderRootSignal::EndIme
            | RenderRootSignal::ImeMoved(..)) => {
                self.text.receive(&signal);
                wake();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::wake::WakeSignal;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn ui_redraws_coalesce_without_leaving_a_kernel_wake_after_render() {
        let signals = Signals::default();
        assert!(!signals.visible.get());
        for _ in 0..4 {
            signals.receive(RenderRootSignal::RequestRedraw, || {
                panic!("local redraw woke the host")
            });
            signals.receive(RenderRootSignal::RequestAnimFrame, || {
                panic!("local animation woke the host")
            });
        }
        assert!(
            signals.take_redraw(),
            "the render phase receives the coalesced work"
        );
        assert!(
            !signals.has_redraw(),
            "the completed render permits the timed wait"
        );
        assert!(!signals.take_redraw());
    }

    #[test]
    fn redraw_requested_during_render_gets_another_turn_before_sleep() {
        let signals = Signals::default();
        signals.request_redraw();
        assert!(signals.take_redraw());
        signals.receive(RenderRootSignal::RequestRedraw, || {
            panic!("render self-wake")
        });
        assert!(
            signals.has_redraw(),
            "the sleep boundary must observe new local work"
        );
        assert!(signals.take_redraw());
        assert!(!signals.has_redraw());
    }

    #[test]
    fn local_redraw_consumption_preserves_racing_background_completion_tokens() {
        for before_render in [false, true] {
            let notified = Arc::new(AtomicBool::new(false));
            let notify = notified.clone();
            let completion = WakeSignal::new(move || notify.store(true, Ordering::Release));
            let signals = Signals::default();
            signals.request_redraw();
            if before_render {
                completion.wake();
            }
            assert!(signals.take_redraw());
            if !before_render {
                completion.wake();
            }
            assert!(!signals.has_redraw());
            assert!(
                completion.is_pending(),
                "IO/media/display/power keeps its pending work"
            );
            assert!(
                notified.load(Ordering::Acquire),
                "the native wait still sees the retained token"
            );
        }
    }

    #[test]
    fn exit_and_ime_keep_their_existing_next_turn_notification() {
        let signals = Signals::default();
        let wakes = Cell::new(0);
        for signal in [
            RenderRootSignal::StartIme,
            RenderRootSignal::EndIme,
            RenderRootSignal::Exit,
        ] {
            signals.receive(signal, || wakes.set(wakes.get() + 1));
        }
        signals.receive(RenderRootSignal::SetTitle("Reel".into()), || {
            panic!("title self-wake")
        });
        assert_eq!(wakes.get(), 3);
        assert!(signals.exit.get());
        assert_eq!(*signals.title.borrow(), "Reel");
        assert!(!signals.has_redraw());
    }
}

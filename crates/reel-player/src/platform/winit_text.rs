//! Native IME and clipboard access stay in the window adapter.
use super::WinitHost;
use reel_ui::{app::RenderRootSignal, core::TextEvent};
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use std::cell::RefCell;
use std::{cell::Cell, rc::Rc, sync::Arc};

#[derive(Clone, Default)]
pub(super) struct TextInputHost {
    enabled: Rc<Cell<bool>>,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    clipboard: Rc<RefCell<Option<arboard::Clipboard>>>,
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
impl TextInputHost {
    fn clipboard<T>(
        &self,
        use_clipboard: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
    ) -> Result<T, arboard::Error> {
        let mut clipboard = self.clipboard.borrow_mut();
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new()?);
        }
        use_clipboard(clipboard.as_mut().unwrap())
    }
}

impl WinitHost {
    /// Install this as the RenderRoot sink. Other hosts consume the same portable signals.
    pub fn ui_signal_handler(&self) -> impl FnMut(RenderRootSignal) + 'static {
        let window = Arc::downgrade(&self.window);
        let text = self.text.clone();
        move |signal| {
            let Some(window) = window.upgrade() else {
                return;
            };
            match signal {
                RenderRootSignal::StartIme => {
                    text.enabled.set(true);
                    window.set_ime_allowed(true);
                }
                RenderRootSignal::EndIme => {
                    text.enabled.set(false);
                    window.set_ime_allowed(false);
                }
                RenderRootSignal::ImeMoved(position, size) => window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(position.x, position.y),
                    winit::dpi::LogicalSize::new(size.width, size.height),
                ),
                #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
                RenderRootSignal::ClipboardStore(value) => {
                    if let Err(error) = text.clipboard(|clipboard| clipboard.set_text(value)) {
                        eprintln!("clipboard copy: {error}");
                    }
                }
                RenderRootSignal::SetCursor(cursor) => window.set_cursor(cursor),
                RenderRootSignal::SetCursorVisible(visible) => window.set_cursor_visible(visible),
                _ => {}
            }
        }
    }

    /// Resolve desktop paste only for an active text editor, on a real paste command.
    pub fn clipboard_paste(&self, event: &TextEvent) -> Option<TextEvent> {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        {
            use reel_ui::core::keyboard::Key;
            if self.text.enabled.get()
                && let TextEvent::Keyboard(key) = event
                && key.state.is_down()
                && !key.modifiers.alt()
                && (if cfg!(target_os = "macos") {
                    key.modifiers.meta()
                } else {
                    key.modifiers.ctrl()
                })
                && let Key::Character(character) = &key.key
                && character.eq_ignore_ascii_case("v")
            {
                match self.text.clipboard(arboard::Clipboard::get_text) {
                    Ok(text) => return Some(TextEvent::ClipboardPaste(text)),
                    Err(arboard::Error::ContentNotAvailable) => {}
                    Err(error) => eprintln!("clipboard paste: {error}"),
                }
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let _ = event;
        None
    }
}

/// Platform conversion contains no editor or language-specific behavior.
pub fn ime(event: winit::event::Ime) -> TextEvent {
    use reel_ui::core::Ime;
    TextEvent::Ime(match event {
        winit::event::Ime::Enabled => Ime::Enabled,
        winit::event::Ime::Disabled => Ime::Disabled,
        winit::event::Ime::Preedit(text, cursor) => Ime::Preedit(text, cursor),
        winit::event::Ime::Commit(text) => Ime::Commit(text),
    })
}

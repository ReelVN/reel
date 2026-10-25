//! Raw system-keyboard events join the existing portable text-edit transaction.
use reel_ui::{
    app::RenderRootSignal,
    core::{
        Ime, TextEvent,
        keyboard::{Key, NamedKey},
    },
};
use std::{cell::Cell, sync::Arc};
type Error = Box<dyn std::error::Error>;

// A preceding unit lets the native keyboard report Backspace even when its
// current composing segment is empty. SDL's Vita PVR adapter uses this same
// raw-keyboard convention; committed text is owned by Reel's editor.
const PREFIX: &str = "\u{1}";

pub(crate) struct Signals {
    enabled: Cell<bool>,
    generation: Cell<u64>,
    area: Cell<[u32; 3]>,
}
impl Default for Signals {
    fn default() -> Self {
        Self {
            enabled: Cell::new(false),
            generation: Cell::new(0),
            area: Cell::new([0, 0, 1]),
        }
    }
}
impl Signals {
    pub(crate) fn receive(&self, signal: &RenderRootSignal) {
        match signal {
            RenderRootSignal::StartIme | RenderRootSignal::EndIme => {
                self.enabled
                    .set(matches!(signal, RenderRootSignal::StartIme));
                self.generation.set(self.generation.get().wrapping_add(1));
            }
            RenderRootSignal::ImeMoved(position, size) => {
                self.area.set([
                    position.x.max(0.) as u32,
                    position.y.max(0.) as u32,
                    size.height.max(1.) as u32,
                ]);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
pub(crate) struct Update {
    pub(crate) text: Option<(Vec<u16>, u32, bool)>,
    pub(crate) enter: bool,
    pub(crate) close: bool,
}
pub(crate) trait Keyboard: Sized {
    fn open() -> Result<Self, Error>;
    fn poll(&mut self) -> Result<Update, Error>;
    fn reset(&mut self) -> Result<(), Error>;
    fn area(&mut self, area: [u32; 3]) -> Result<(), Error>;
    fn close(&mut self) -> Result<(), Error>;
}

pub(crate) struct TextInput<K> {
    keyboard: Option<K>,
    generation: u64,
    dismissed: Option<u64>,
    area: [u32; 3],
    composing: Arc<str>,
}
impl<K: Keyboard> Default for TextInput<K> {
    fn default() -> Self {
        Self {
            keyboard: None,
            generation: 0,
            dismissed: None,
            area: [0; 3],
            composing: Arc::from(""),
        }
    }
}
impl<K: Keyboard> TextInput<K> {
    pub(crate) fn active(&self) -> bool {
        self.keyboard.is_some()
    }

    pub(crate) fn cancel(
        &mut self,
        mut emit: impl FnMut(TextEvent) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.close()?;
            self.keyboard = None;
            self.composing = Arc::from("");
            emit(TextEvent::Ime(Ime::Disabled))?;
        }
        Ok(())
    }

    pub(crate) fn update(
        &mut self,
        signals: &Signals,
        mut emit: impl FnMut(TextEvent) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let generation = signals.generation.get();
        if generation != self.generation || !signals.enabled.get() {
            self.cancel(&mut emit)?;
            self.generation = generation;
        }
        if !signals.enabled.get() || self.dismissed == Some(generation) {
            return Ok(());
        }
        if self.keyboard.is_none() {
            let mut keyboard = K::open()?;
            self.area = signals.area.get();
            keyboard.area(self.area)?;
            self.keyboard = Some(keyboard);
            emit(TextEvent::Ime(Ime::Enabled))?;
        }
        if signals.generation.get() != generation {
            return Ok(());
        }
        let keyboard = self.keyboard.as_mut().unwrap();
        if self.area != signals.area.get() {
            self.area = signals.area.get();
            keyboard.area(self.area)?;
        }
        let update = keyboard.poll()?;
        let mut events = Vec::new();
        if let Some((units, caret, composing)) = update.text {
            let prefix = usize::from(units.first() == Some(&1));
            let caret = (caret as usize).checked_sub(prefix).unwrap_or(0);
            let units = &units[prefix..];
            if caret > units.len() {
                return Err("system keyboard caret exceeds its text".into());
            }
            let text: Arc<str> = String::from_utf16(units)?.into();
            let cursor = String::from_utf16(&units[..caret])?.len();
            if composing {
                self.composing = text.clone();
                events.push(TextEvent::Ime(Ime::Preedit(text, Some((cursor, cursor)))));
            } else if !text.is_empty() {
                self.composing = Arc::from("");
                events.push(TextEvent::Ime(Ime::Preedit(Arc::from(""), None)));
                events.push(TextEvent::Ime(Ime::Commit(text)));
                keyboard.reset()?;
            } else if prefix == 0 {
                self.composing = Arc::from("");
                events.push(TextEvent::Ime(Ime::Preedit(Arc::from(""), None)));
                events.push(TextEvent::key_down(Key::Named(NamedKey::Backspace)));
                events.push(TextEvent::key_up(Key::Named(NamedKey::Backspace)));
                keyboard.reset()?;
            } else if !self.composing.is_empty() {
                self.composing = Arc::from("");
                events.push(TextEvent::Ime(Ime::Preedit(Arc::from(""), None)));
            }
        }
        if update.enter {
            if !self.composing.is_empty() {
                events.push(TextEvent::Ime(Ime::Preedit(Arc::from(""), None)));
                events.push(TextEvent::Ime(Ime::Commit(std::mem::replace(
                    &mut self.composing,
                    Arc::from(""),
                ))));
            }
            events.push(TextEvent::key_down(Key::Named(NamedKey::Enter)));
            events.push(TextEvent::key_up(Key::Named(NamedKey::Enter)));
        }
        for event in events {
            if signals.generation.get() != generation || !signals.enabled.get() {
                break;
            }
            emit(event)?;
        }
        if update.close || update.enter {
            self.dismissed = Some(generation);
            self.cancel(emit)?;
        }
        Ok(())
    }
}

#[cfg(target_os = "vita")]
impl Keyboard for pvr_psp2_sys::platform::ime::Keyboard {
    fn open() -> Result<Self, Error> {
        Ok(Self::open(PREFIX)?)
    }
    fn poll(&mut self) -> Result<Update, Error> {
        let update = self.poll()?;
        Ok(Update {
            text: update
                .text
                .map(|text| (text.value, text.caret, text.preedit_length != 0)),
            enter: update.enter,
            close: update.close,
        })
    }
    fn reset(&mut self) -> Result<(), Error> {
        Ok(self.set_text(PREFIX, 1)?)
    }
    fn area(&mut self, area: [u32; 3]) -> Result<(), Error> {
        Ok(self.set_area(area)?)
    }
    fn close(&mut self) -> Result<(), Error> {
        Ok(self.close()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};
    #[derive(Default)]
    struct State {
        updates: VecDeque<Update>,
        opened: usize,
        closed: usize,
        resets: usize,
        fail_close: bool,
    }
    thread_local! { static STATE: RefCell<State> = RefCell::default(); }
    struct Fake;
    impl Keyboard for Fake {
        fn open() -> Result<Self, Error> {
            STATE.with_borrow_mut(|state| state.opened += 1);
            Ok(Self)
        }
        fn poll(&mut self) -> Result<Update, Error> {
            Ok(STATE.with_borrow_mut(|state| state.updates.pop_front().unwrap_or_default()))
        }
        fn reset(&mut self) -> Result<(), Error> {
            STATE.with_borrow_mut(|state| state.resets += 1);
            Ok(())
        }
        fn area(&mut self, _: [u32; 3]) -> Result<(), Error> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), Error> {
            STATE.with_borrow_mut(|state| {
                if std::mem::take(&mut state.fail_close) {
                    return Err("native close failed: 0x80100704".into());
                }
                state.closed += 1;
                state.updates.clear();
                Ok(())
            })
        }
    }
    fn fixture() -> (TextInput<Fake>, Signals) {
        STATE.with_borrow_mut(|state| *state = State::default());
        let signals = Signals::default();
        signals.receive(&RenderRootSignal::StartIme);
        (TextInput::default(), signals)
    }
    fn queue(text: &str, composing: bool) {
        let units: Vec<_> = text.encode_utf16().collect();
        STATE.with_borrow_mut(|state| {
            state.updates.push_back(Update {
                text: Some((units.clone(), units.len() as u32, composing)),
                ..Default::default()
            })
        });
    }
    fn pump(input: &mut TextInput<Fake>, signals: &Signals) -> Vec<TextEvent> {
        let mut events = Vec::new();
        input
            .update(signals, |event| {
                events.push(event);
                Ok(())
            })
            .unwrap();
        events
    }
    #[test]
    fn unicode_composition_confirm_and_backspace_use_portable_edits() {
        let (mut input, signals) = fixture();
        assert_eq!(pump(&mut input, &signals), [TextEvent::Ime(Ime::Enabled)]);
        queue(&format!("{PREFIX}中😀"), true);
        assert_eq!(
            pump(&mut input, &signals),
            [TextEvent::Ime(Ime::Preedit("中😀".into(), Some((7, 7))))]
        );
        queue(PREFIX, false);
        assert_eq!(
            pump(&mut input, &signals),
            [TextEvent::Ime(Ime::Preedit("".into(), None))]
        );
        queue(&format!("{PREFIX}é"), false);
        assert_eq!(
            pump(&mut input, &signals),
            [
                TextEvent::Ime(Ime::Preedit("".into(), None)),
                TextEvent::Ime(Ime::Commit("é".into()))
            ]
        );
        queue("", false);
        let backspace = pump(&mut input, &signals);
        assert!(
            matches!(&backspace[1], TextEvent::Keyboard(key) if key.key == Key::Named(NamedKey::Backspace) && key.state.is_down())
        );
        queue(&format!("{PREFIX}新"), true);
        pump(&mut input, &signals);
        STATE.with_borrow_mut(|state| {
            state.updates.push_back(Update {
                enter: true,
                ..Default::default()
            })
        });
        let entered = pump(&mut input, &signals);
        assert_eq!(entered[1], TextEvent::Ime(Ime::Commit("新".into())));
        assert!(
            matches!(&entered[2], TextEvent::Keyboard(key) if key.key == Key::Named(NamedKey::Enter) && key.state.is_down())
        );
        assert_eq!(entered.last(), Some(&TextEvent::Ime(Ime::Disabled)));
        assert!(!input.active());
        assert!(pump(&mut input, &signals).is_empty());
        STATE
            .with_borrow(|state| assert_eq!((state.opened, state.closed, state.resets), (1, 1, 2)));
    }
    #[test]
    fn focus_restarts_drop_stale_text_and_cancel_retains_failed_close() {
        let (mut input, signals) = fixture();
        pump(&mut input, &signals);
        queue(&format!("{PREFIX}stale"), false);
        signals.receive(&RenderRootSignal::EndIme);
        signals.receive(&RenderRootSignal::StartIme);
        assert_eq!(
            pump(&mut input, &signals),
            [TextEvent::Ime(Ime::Disabled), TextEvent::Ime(Ime::Enabled)]
        );
        queue(&format!("{PREFIX}stale"), false);
        let mut events = Vec::new();
        input
            .update(&signals, |event| {
                signals.receive(&RenderRootSignal::EndIme);
                events.push(event);
                Ok(())
            })
            .unwrap();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, TextEvent::Ime(Ime::Commit(_))))
        );
        STATE.with_borrow_mut(|state| state.fail_close = true);
        let error = input.cancel(|_| Ok(())).unwrap_err().to_string();
        assert!(error.contains("0x80100704"));
        assert!(input.active());
        input.cancel(|_| Ok(())).unwrap();
        assert!(!input.active());
        assert!(pump(&mut input, &signals).is_empty());
        STATE.with_borrow(|state| assert_eq!((state.opened, state.closed), (2, 2)));
    }
    #[test]
    fn close_discards_preedit_and_a_new_focus_request_reopens() {
        let (mut input, signals) = fixture();
        pump(&mut input, &signals);
        queue(&format!("{PREFIX}候选"), true);
        pump(&mut input, &signals);
        STATE.with_borrow_mut(|state| {
            state.updates.push_back(Update {
                close: true,
                ..Default::default()
            })
        });
        assert_eq!(pump(&mut input, &signals), [TextEvent::Ime(Ime::Disabled)]);
        assert!(pump(&mut input, &signals).is_empty());
        signals.receive(&RenderRootSignal::EndIme);
        signals.receive(&RenderRootSignal::StartIme);
        assert_eq!(pump(&mut input, &signals), [TextEvent::Ime(Ime::Enabled)]);
        input.cancel(|_| Ok(())).unwrap();
        STATE.with_borrow(|state| assert_eq!((state.opened, state.closed), (2, 2)));
    }
}

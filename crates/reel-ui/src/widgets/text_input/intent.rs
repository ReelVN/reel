//! Portable accepted editing intents, replayed by the same layout-aware editor.
use super::*;

/// Bounds apply only to accepted events waiting for an asynchronous edit operation.
/// Ordinary typing and a normal large paste do not enter this queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextInputEditingLimits {
    pub max_events: usize,
    pub max_text_bytes: usize,
}
impl Default for TextInputEditingLimits {
    fn default() -> Self {
        Self {
            max_events: 256,
            max_text_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextInputKey {
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// A semantic editing event keeps relative keyboard operations unresolved until replay.
/// This preserves visual navigation, IME and clipboard behavior in the shared editor.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TextInputIntent {
    Character {
        text: Arc<str>,
        modifiers: u32,
        repeat: bool,
    },
    Key {
        key: TextInputKey,
        modifiers: u32,
        repeat: bool,
    },
    Paste(Arc<str>),
    ReplaceAll(Arc<str>),
    Commit(Arc<str>),
    Preedit {
        text: Arc<str>,
        cursor: Option<(usize, usize)>,
    },
    Selection {
        selection: TextSelection,
    },
}

impl TextInputIntent {
    pub fn text_bytes(&self) -> usize {
        match self {
            Self::Character { text, .. }
            | Self::Preedit { text, .. }
            | Self::Paste(text)
            | Self::ReplaceAll(text)
            | Self::Commit(text) => text.len(),
            _ => 0,
        }
    }

    pub(super) fn capture(event: &TextEvent) -> Option<Self> {
        match event {
            TextEvent::Keyboard(event) if event.state.is_down() => {
                let modifiers = event.modifiers.bits();
                let repeat = event.repeat;
                Some(match &event.key {
                    Key::Character(text) => Self::Character {
                        text: text.as_str().into(),
                        modifiers,
                        repeat,
                    },
                    Key::Named(key) => Self::Key {
                        key: match key {
                            NamedKey::Enter => TextInputKey::Enter,
                            NamedKey::Backspace => TextInputKey::Backspace,
                            NamedKey::Delete => TextInputKey::Delete,
                            NamedKey::ArrowLeft => TextInputKey::Left,
                            NamedKey::ArrowRight => TextInputKey::Right,
                            NamedKey::ArrowUp => TextInputKey::Up,
                            NamedKey::ArrowDown => TextInputKey::Down,
                            NamedKey::Home => TextInputKey::Home,
                            NamedKey::End => TextInputKey::End,
                            _ => return None,
                        },
                        modifiers,
                        repeat,
                    },
                })
            }
            TextEvent::ClipboardPaste(text) => Some(Self::Paste(text.clone())),
            TextEvent::Ime(Ime::Commit(text)) => Some(Self::Commit(text.clone())),
            TextEvent::Ime(Ime::Preedit(text, cursor)) => Some(Self::Preedit {
                text: text.clone(),
                cursor: *cursor,
            }),
            _ => None,
        }
    }

    pub(super) fn event(&self) -> Option<TextEvent> {
        let (key, modifiers, repeat) = match self {
            Self::Character {
                text,
                modifiers,
                repeat,
            } => (Key::Character(text.as_ref().into()), *modifiers, *repeat),
            Self::Key {
                key,
                modifiers,
                repeat,
            } => (
                Key::Named(match key {
                    TextInputKey::Enter => NamedKey::Enter,
                    TextInputKey::Backspace => NamedKey::Backspace,
                    TextInputKey::Delete => NamedKey::Delete,
                    TextInputKey::Left => NamedKey::ArrowLeft,
                    TextInputKey::Right => NamedKey::ArrowRight,
                    TextInputKey::Up => NamedKey::ArrowUp,
                    TextInputKey::Down => NamedKey::ArrowDown,
                    TextInputKey::Home => NamedKey::Home,
                    TextInputKey::End => NamedKey::End,
                }),
                *modifiers,
                *repeat,
            ),
            Self::Paste(text) => return Some(TextEvent::ClipboardPaste(text.clone())),
            Self::Commit(text) => return Some(TextEvent::Ime(Ime::Commit(text.clone()))),
            Self::Preedit { text, cursor } => {
                return Some(TextEvent::Ime(Ime::Preedit(text.clone(), *cursor)));
            }
            Self::Selection { .. } | Self::ReplaceAll(_) => return None,
        };
        let TextEvent::Keyboard(mut event) = TextEvent::key_down(key) else {
            unreachable!()
        };
        event.modifiers = keyboard::Modifiers::from_bits_retain(modifiers);
        event.repeat = repeat;
        Some(TextEvent::Keyboard(event))
    }
}

/// A node's queue generation and its ordered event identity survive save restoration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TextInputReplayId {
    pub generation: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TextInputReplay {
    pub id: TextInputReplayId,
    pub intent: Arc<TextInputIntent>,
    // Ownership follows the last queue/projection reference, including an old UI frame.
    #[serde(skip)]
    _retention: Option<Arc<reel_core::budget::BudgetTicket>>,
}
impl TextInputReplay {
    pub fn new(
        id: TextInputReplayId,
        intent: Arc<TextInputIntent>,
        retention: Arc<reel_core::budget::BudgetTicket>,
    ) -> Self {
        Self {
            id,
            intent,
            _retention: Some(retention),
        }
    }
}
impl PartialEq for TextInputReplay {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.intent == other.intent
    }
}

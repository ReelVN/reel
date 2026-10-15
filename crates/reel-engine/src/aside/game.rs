//! Script-owned presentation settings, consumed by the embedding host.
use super::{AdapterError, HostTicket, SessionVm, text};
use aside_vm::{Value, ValueTag, executor::HostResponse};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GameSettings {
    pub title: String,
    pub canvas_size: Option<[u32; 2]>,
    pub fonts: Vec<String>,
    pub text_speed: f64,
    pub source: Option<String>,
}
impl Default for GameSettings {
    fn default() -> Self {
        Self {
            title: "Reel".into(),
            canvas_size: None,
            fonts: Vec::new(),
            text_speed: 30.,
            source: None,
        }
    }
}
impl SessionVm {
    /// The immutable value changes identity only when a setting changes.
    pub fn game_settings(&self) -> &Arc<GameSettings> {
        &self.game
    }

    /// Carry host-visible configuration into a fresh story session.
    pub fn inherit_game_settings(&mut self, settings: Arc<GameSettings>) {
        self.game = settings;
    }

    pub(super) fn configure_game(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let mut next = (*self.game).clone();
        let read = |value| text::read_string(self.vm.heap(), value, self.read_limit);
        if args[0].tag() != ValueTag::Null {
            next.title = read(args[0])?;
        }
        if args[1].tag() != ValueTag::Null || args[2].tag() != ValueTag::Null {
            let mut size = next.canvas_size.unwrap_or([1920, 1080]);
            for i in 0..2 {
                if args[i + 1].tag() != ValueTag::Null {
                    size[i] = args[i + 1]
                        .as_int()
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|&v| v > 0)
                        .ok_or(AdapterError::Input(
                            "canvas dimensions must be positive integers",
                        ))?;
                }
            }
            next.canvas_size = Some(size);
        }
        if args[3].tag() != ValueTag::Null {
            let heap = self.vm.heap();
            next.fonts = if heap.info(args[3])?.kind == aside_vm::ObjectKind::String {
                vec![read(args[3])?]
            } else {
                (0..heap.sequence_len(args[3])?)
                    .map(|i| read(heap.sequence_get(args[3], i)?))
                    .collect::<Result<_, AdapterError>>()?
            };
            if next.fonts.is_empty() || next.fonts.iter().any(String::is_empty) {
                return Err(AdapterError::Input("provide at least one font file"));
            }
        }
        if args[4].tag() != ValueTag::Null {
            let speed = args[4]
                .as_float()
                .or_else(|| args[4].as_int().map(|n| n as f64))
                .filter(|n| n.is_finite() && *n >= 0.)
                .ok_or(AdapterError::Input(
                    "text_speed must be finite and nonnegative",
                ))?;
            next.text_speed = speed;
        }
        if next != *self.game {
            if next.fonts != self.game.fonts {
                let p = self.vm.program().package();
                next.source = ticket.location.source.map(|span| {
                    let source = &p.sources[span.file.index()];
                    let before = &source.text[..span.start as usize];
                    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
                    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
                    format!("{}:{line}:{column}", p.symbols[source.path.index()])
                });
            }
            self.game = Arc::new(next);
        }
        self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
        self.issued = None;
        Ok(())
    }
}

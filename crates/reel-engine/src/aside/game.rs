//! Script-owned presentation settings, consumed by the embedding host.
use super::{AdapterError, HostTicket, SessionVm, text};

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Set title, logical canvas size, font assets, text speed and animation rate. Omitted values
        /// remain unchanged. Register fonts before first presentation. The canvas uses
        /// contain scaling and one transform for drawing and input.
        /// `title` accepts string or null; `width` and `height` accept positive integers
        /// or null; `text_speed` accepts nonnegative numeric values or null.
        /// `fonts` accepts one asset-path string, an array or tuple of asset-path strings,
        /// or null.
        /// `frame_rate` accepts 1..1000 Hz, rounded to 0.001 Hz; 0 selects automatic
        /// display refresh rate (60 Hz when unknown). The default is automatic.
        /// It controls tweens, particles and transitions; explicit clock intervals,
        /// text reveal speed and window vsync retain their own settings.
        "reel::game::configure";
        /// Replace the current session with a fresh invocation of entry. Persistent
        /// storage remains available; tasks and transient stage state belong to the new session.
        "reel::game::start";
        /// Request that the host close this game session.
        "reel::game::exit";
    }
}

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{nullable, number};
    // Fonts accept strings and sequences of arbitrary tuple length. The current
    // hints express fixed tuple shapes, so retain runtime checking for this argument.
    api.refine(
        "reel::game::configure",
        TypeHint::Null,
        &[
            ("title", nullable(TypeHint::String)),
            ("width", nullable(TypeHint::Int)),
            ("height", nullable(TypeHint::Int)),
            ("text_speed", nullable(number())),
            ("frame_rate", nullable(number())),
        ],
    );
    api.refine(
        "reel::game::start",
        TypeHint::Null,
        &[("entry", TypeHint::Closure)],
    );
    api.refine("reel::game::exit", TypeHint::Null, &[]);
}
use aside_vm::{Value, ValueTag, executor::HostResponse};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GameSettings {
    pub title: String,
    pub canvas_size: Option<[u32; 2]>,
    pub fonts: Vec<String>,
    pub text_speed: f64,
    /// Continuous animation sampling rate in millihertz; None follows the display.
    pub frame_rate_millihertz: Option<u32>,
    pub source: Option<String>,
}
impl Default for GameSettings {
    fn default() -> Self {
        Self {
            title: "Reel".into(),
            canvas_size: None,
            fonts: Vec::new(),
            text_speed: 30.,
            frame_rate_millihertz: None,
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
        if args[5].tag() != ValueTag::Null {
            let rate = args[5]
                .as_float()
                .or_else(|| args[5].as_int().map(|n| n as f64))
                .filter(|rate| rate.is_finite() && (*rate == 0. || (1.0..=1000.).contains(rate)))
                .ok_or(AdapterError::Input(
                    "frame_rate must be 0 (automatic) or finite Hz in 1..1000",
                ))?;
            next.frame_rate_millihertz = (rate != 0.).then_some((rate * 1000.).round() as u32);
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

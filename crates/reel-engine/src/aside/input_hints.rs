//! Observable presentation hints for the host's current input devices.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable};
use aside_vm::{RootKind, RootToken};
use reel_ui::input::InputHints;

#[derive(Default)]
pub(super) struct State {
    pub(super) root: Option<RootToken>,
    current: InputHints,
}
const TYPE: &str = "reel::types::InputInfo";
aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Return shared read-only input hints. Fields are observable, so bind or
        /// derive can select game-owned icons when the input source changes.
        /// Connections, releases and navigation repeats do not select a new source.
        /// Gamepad axes and analog buttons must exceed the 0.2 hint dead zone.
        "reel::stage::input";
    }
}
pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named};
    let fields = [
        (
            "source",
            "Last active input: unknown, keyboard, mouse, touch, pen or gamepad. Initially unknown.",
        ),
        (
            "layout",
            "Recognized gamepad labels: xbox, playstation or nintendo. Generic for unknown controllers and other input sources. The game supplies icons.",
        ),
    ];
    api.types.push(NamedType::new(
        TYPE,
        "Current host input hints; refreshed after loading a save.",
        fields
            .into_iter()
            .map(|(name, doc)| Member::field(TYPE, name, TypeHint::String, None, doc, None))
            .collect(),
    ));
    api.refine("reel::stage::input", named(TYPE), &[]);
}
impl SessionVm {
    /// Publish only changed hint fields; ordinary input retains the same VM object.
    pub fn set_input_hints(&mut self, hints: InputHints) -> Result<bool, AdapterError> {
        if self.input_hints.current == hints {
            return Ok(false);
        }
        if let Some(root) = self.input_hints.root {
            let heap = self.vm.heap_mut();
            let value = heap.root_value(root)?;
            for (index, (before, after)) in [
                (
                    self.input_hints.current.source.as_str(),
                    hints.source.as_str(),
                ),
                (
                    self.input_hints.current.layout.as_str(),
                    hints.layout.as_str(),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                if before != after {
                    let text = heap.alloc_string(after)?;
                    observable::publish(heap, value, index as u32, text)?;
                }
            }
        }
        self.input_hints.current = hints;
        Ok(true)
    }
    pub(super) fn reset_input_hints(&mut self) -> Result<(), AdapterError> {
        // Saved references keep their identity; current device state belongs to the host.
        if let Some(root) = self.input_hints.root {
            let heap = self.vm.heap_mut();
            let value = heap.root_value(root)?;
            for (field, text) in ["unknown", "generic"].into_iter().enumerate() {
                let text = heap.alloc_string(text)?;
                observable::publish(heap, value, field as u32, text)?;
            }
        }
        self.input_hints.current = InputHints::default();
        Ok(())
    }
    pub(super) fn dispatch_input_hints(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let heap = self.vm.heap_mut();
        let value = if let Some(root) = self.input_hints.root {
            heap.root_value(root)?
        } else {
            let source = heap.alloc_string(self.input_hints.current.source.as_str())?;
            let layout = heap.alloc_string(self.input_hints.current.layout.as_str())?;
            let value = observable::record(heap, &[("source", source), ("layout", layout)], &[])?;
            self.stage.types.mark(heap, value, TYPE)?;
            self.input_hints.root = Some(heap.add_root(self.root_owner, RootKind::Host, value)?);
            value
        };
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }
}

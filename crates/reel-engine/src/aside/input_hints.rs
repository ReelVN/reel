//! Observable presentation hints for the host's current input devices.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable};
use aside_vm::{Heap, RootKind, RootToken, Value};
use reel_ui::input::{GamepadLayout, InputHints};

#[derive(Default)]
pub(super) struct State {
    pub(super) root: Option<RootToken>,
    current: InputHints,
}
const TYPE: &str = "reel::types::InputInfo";
const LABELS: &str = "reel::types::GamepadLabels";
const BUTTONS: [(&str, &str); 10] = [
    (
        "South",
        "Label of the bottom face button, such as A on Xbox or Cross on PlayStation.",
    ),
    (
        "East",
        "Label of the right face button, such as B on Xbox or Circle on PlayStation.",
    ),
    (
        "North",
        "Label of the top face button, such as Y on Xbox or Triangle on PlayStation.",
    ),
    (
        "West",
        "Label of the left face button, such as X on Xbox or Square on PlayStation.",
    ),
    (
        "LeftShoulder",
        "Label of the left shoulder button, such as LB or L1.",
    ),
    (
        "RightShoulder",
        "Label of the right shoulder button, such as RB or R1.",
    ),
    (
        "LeftTrigger",
        "Label of the left trigger, such as LT or L2.",
    ),
    (
        "RightTrigger",
        "Label of the right trigger, such as RT or R2.",
    ),
    (
        "Select",
        "Label of the select button, such as View on Xbox or minus on Nintendo.",
    ),
    (
        "Start",
        "Label of the start button, such as Menu on Xbox or plus on Nintendo.",
    ),
];

fn buttons(heap: &mut Heap, layout: GamepadLayout) -> Result<Value, AdapterError> {
    let labels = match layout {
        GamepadLayout::Xbox => ["A", "B", "Y", "X", "LB", "RB", "LT", "RT", "View", "Menu"],
        GamepadLayout::PlayStation => [
            "Cross", "Circle", "Triangle", "Square", "L1", "R1", "L2", "R2", "Select", "Start",
        ],
        GamepadLayout::Nintendo => ["B", "A", "X", "Y", "L", "R", "ZL", "ZR", "−", "+"],
        GamepadLayout::Generic => BUTTONS.map(|(name, _)| name),
    };
    let mut fields = [("", Value::NULL); BUTTONS.len()];
    for ((field, (name, _)), label) in fields.iter_mut().zip(BUTTONS).zip(labels) {
        *field = (name, heap.alloc_string(label)?);
    }
    observable::record(heap, &fields, &[])
}
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
    api.types.push(NamedType::new(
        LABELS,
        "Read-only display labels indexed by logical gamepad button name.",
        BUTTONS
            .into_iter()
            .map(|(button, documentation)| {
                Member::field(LABELS, button, TypeHint::String, None, documentation, None)
            })
            .collect(),
    ));
    let mut fields: Vec<_> = [
        (
            "source",
            "Last active input: unknown, keyboard, mouse, touch, pen or gamepad. Initially unknown.",
        ),
        (
            "layout",
            "Recognized gamepad labels: xbox, playstation or nintendo. Generic for unknown controllers and other input sources. The game supplies icons.",
        ),
    ].into_iter()
        .map(|(name, doc)| Member::field(TYPE, name, TypeHint::String, None, doc, None))
        .collect();
    fields.push(Member::field(
        TYPE, "buttons", named(LABELS), None,
        "Gamepad display labels for the active layout. Bind through this field to update when the layout changes.",
        None,
    ));
    api.types.push(NamedType::new(
        TYPE,
        "Current host input hints; refreshed after loading a save.",
        fields,
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
            if self.input_hints.current.layout != hints.layout {
                let labels = buttons(heap, hints.layout)?;
                self.stage.types.mark(heap, labels, LABELS)?;
                observable::publish(heap, value, 2, labels)?;
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
            let labels = buttons(heap, GamepadLayout::Generic)?;
            self.stage.types.mark(heap, labels, LABELS)?;
            observable::publish(heap, value, 2, labels)?;
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
            let labels = buttons(heap, self.input_hints.current.layout)?;
            self.stage.types.mark(heap, labels, LABELS)?;
            let value = observable::record(
                heap,
                &[("source", source), ("layout", layout), ("buttons", labels)],
                &[],
            )?;
            self.stage.types.mark(heap, value, TYPE)?;
            self.input_hints.root = Some(heap.add_root(self.root_owner, RootKind::Host, value)?);
            value
        };
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }
}

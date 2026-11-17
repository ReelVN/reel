//! Context-owned observable narrative data, independent of its presentation.
mod contexts;
mod history;
mod projection;
pub(super) use contexts::Contexts;
pub(in crate::aside) use projection::ReadingProjection;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Present one dialogue line on the main story task. `history` accepts true, false,
        /// or replacement text. With `wait: false`, return a speech handle immediately;
        /// call its `append` and `wait` methods to stage the line in segments.
        /// Reveal input completes visible text before advancing an armed wait.
        "reel::narrator" | "reel::character::speak";
        /// Create a speaker with a display name and an optional stable id.
        /// Calling the speaker presents a line; `.line(text)` creates data for `say`.
        "reel::character";
        /// Create immutable line data for `say`, preserving this speaker and history text.
        /// This prepares content without starting a dialogue interaction.
        "reel::character::line";
        /// Present an array of character line values as one dialogue interaction.
        /// All lines share reveal and advance; `wait: false` returns its speech handle.
        /// Use the observable dialogue lines to construct the visual arrangement.
        "reel::say";
        /// Append plain or rich text to the current single-speaker speech handle.
        /// Keep its already revealed prefix and history entry. `history` selects the
        /// appended history text. Wait for advance by default; `wait: false` returns
        /// immediately for sounds or choreography before the next advance point.
        /// Return the same Speech in either case so further operations can be chained.
        /// A newer dialogue or choice retires the handle.
        "reel::speech::append";
        /// Arm and wait for advance on the current speech handle. May be called again
        /// after an append. The handle belongs to the main story task and survives saves.
        /// Return the same Speech after the advancement checkpoint.
        "reel::speech::wait";
        /// Wait for this speech's text and inline pauses to finish, then return the same
        /// Speech. This observes playback without arming advance or recording a read
        /// checkpoint. Manual reveal and the normal playback policy continue to apply.
        /// Author-owned visual effects remain separate ordinary activities.
        ///
        /// ```aside
        /// use reel::types::Speech;
        /// fn main() {
        ///     let line: Speech = narrator("Listen.", wait: false);
        ///     line.revealed().append("\nThe wind is quiet.", wait: false).wait();
        /// }
        /// ```
        "reel::speech::revealed";
        /// Return the active reading context's observable dialogue view: lines, text, reveal, active, ready,
        /// speaker_name, speaker_id, content_id, read and advance_requested. Bind these values to ordinary
        /// text nodes. `ready` means printing and inline pauses have completed.
        /// `advance_requested` records an accepted manual advance for the active segment,
        /// including ready text. Protected incomplete reveal and playback do not set it.
        /// Append/new dialogue/clear reset it; snapshots retain it. The root field is
        /// false after the interaction ends. Older displayed rows retain their last
        /// projected value; current_line_ids identifies the active segment's lines.
        /// Capture this object when constructing a component. A later getter,
        /// including an event callback's getter, selects the then-active context.
        "reel::game::dialogue";
        /// Clear displayed lines and ADV fields while retaining the current speech's
        /// wait and advance ownership. History entries are retained. Append displays
        /// the updated speech again; the cleared page survives ticks and snapshots.
        "reel::dialogue::clear";
        /// Return the active reading context's observable choice view with options and active. Call `.select(option.id)`
        /// from an authored control using the opaque id of a current option. The option's
        /// string key is author data; it is not the input identity accepted by select.
        "reel::game::choice";
        /// Present an array of `{key, label}` options and wait on the main story task.
        /// Return the selected string key. Render the options through `game.choice()`.
        "reel::choose";
        /// Submit the opaque `id` of a current choice option. Return true when the input
        /// is accepted, or false for a stale/unrecognized id or disallowed input owner.
        /// The waiting `choose` call returns that option's author-supplied string key.
        "reel::choice::select";
        /// Return the active reading context's observable dialogue history collection.
        /// Entries retain their speaker, text and content identity independently of layout.
        /// Capture the array in a component and refresh from that same array.
        "reel::game::history";
        /// Append a history entry without presenting dialogue. `speaker` optionally
        /// provides its display name; text accepts plain or rich content.
        "reel::game::record";
        /// Return the active reading context's observable controls: auto, paused,
        /// delay, skipping and auto_ready. Direct getters inside derive follow
        /// context switches; captured objects retain their original context.
        /// Capture this context's object in components. Its set_auto, set_skip and
        /// pause methods control that context even while another context is active.
        /// On exit, retained models keep their last values; control operations fail.
        "reel::game::reading";
        /// Pause or resume dialogue reveal and automatic advancement. Other tasks,
        /// animations and media keep running, so menus can own their own behavior.
        /// Return the same Reading. Retiring its context releases this pause.
        "reel::reading::pause";
        /// Configure automatic dialogue advance. Null arguments preserve the current value
        /// or binding. `enabled` accepts bool; `delay` accepts nonnegative finite seconds.
        /// `condition` accepts bool or a bind/derive reference and resolves to auto_ready.
        /// False resets only the automatic countdown; reveal, manual input, skip and
        /// [next] continue normally. True replaces any previous condition binding.
        /// Once ready, wait the full delay after presentation. Bindings survive saves.
        /// Segmented speech advances only at `wait`.
        /// The condition belongs to this context; delay is shared across contexts.
        /// Return the same Reading.
        "reel::reading::set_auto";
        /// Enable or disable dialogue skipping. `unread` permits unread content;
        /// `after_choices` keeps skipping after a choice. Both options default to false.
        /// Return the same Reading.
        "reel::reading::set_skip";
        /// Clear the session's accumulated read-content identities.
        "reel::game::clear_read_progress";
        /// Discard older history entries and retain the last `keep_last` entries.
        /// The default zero clears this context's history. Return the same Reading.
        "reel::reading::clear_history";
    }
}
use super::*;
use crate::{ChoiceId, InputAction, InteractionView};
use aside_vm::{Heap, HeapHandle, PropertyId, ValueTag, bytecode::FunctionKind};

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, nullable, number};
    let text = TypeHint::Union(vec![TypeHint::String, named("reel::types::RichText")]);
    let reading = named("reel::types::Reading");
    api.refine(
        "reel::game::record",
        TypeHint::Null,
        &[
            ("text", text.clone()),
            ("speaker", nullable(named("reel::types::Character"))),
        ],
    );
    api.refine(
        "reel::reading::clear_history",
        reading.clone(),
        &[("reading", reading.clone()), ("keep_last", TypeHint::Int)],
    );
    api.refine(
        "reel::reading::pause",
        reading.clone(),
        &[("reading", reading.clone()), ("paused", TypeHint::Bool)],
    );
    api.refine(
        "reel::reading::set_auto",
        reading.clone(),
        &[
            ("reading", reading.clone()),
            ("enabled", nullable(TypeHint::Bool)),
            ("delay", nullable(number())),
            (
                "condition",
                nullable(TypeHint::Union(vec![TypeHint::Bool, named("Binding")])),
            ),
        ],
    );
    api.refine(
        "reel::reading::set_skip",
        reading.clone(),
        &[
            ("reading", reading),
            ("enabled", TypeHint::Bool),
            ("unread", TypeHint::Bool),
            ("after_choices", TypeHint::Bool),
        ],
    );
    let mut dialogue = Vec::new();
    for (name, ty) in [
        ("lines", TypeHint::Array),
        ("current_line_ids", TypeHint::Array),
        ("speaker_name", TypeHint::String),
        ("text", text),
        ("reveal", TypeHint::Int),
        ("active", TypeHint::Bool),
        ("read", TypeHint::Bool),
        ("speaker_id", nullable(TypeHint::String)),
        ("content_id", nullable(TypeHint::String)),
        ("ready", TypeHint::Bool),
        ("advance_requested", TypeHint::Bool),
    ] {
        dialogue.push(Member::field(
            "reel::types::Dialogue",
            name,
            ty,
            None,
            "The current observable dialogue state.",
            None,
        ));
    }
    dialogue.push(Member::method(
        api.native("reel::dialogue::clear").clone().bound("clear"),
    ));
    api.types.push(NamedType::new(
        "reel::types::Dialogue",
        "A reading context's observable dialogue lines and advancement state.",
        dialogue,
    ));
    let mut choice = vec![
        Member::field(
            "reel::types::Choice",
            "options",
            TypeHint::Array,
            None,
            "Currently available choice options.",
            None,
        ),
        Member::field(
            "reel::types::Choice",
            "active",
            TypeHint::Bool,
            None,
            "Whether a choice is accepting input.",
            None,
        ),
    ];
    choice.push(Member::method(
        api.native("reel::choice::select").clone().bound("select"),
    ));
    api.types.push(NamedType::new(
        "reel::types::Choice",
        "The current observable choice and its selection method.",
        choice,
    ));
    let mut reading: Vec<Member> = [
        "auto",
        "paused",
        "delay",
        "skipping",
        "skip_unread",
        "skip_after_choices",
        "auto_ready",
    ]
    .into_iter()
    .map(|name| {
        Member::field(
            "reel::types::Reading",
            name,
            if name == "delay" {
                TypeHint::Float
            } else {
                TypeHint::Bool
            },
            None,
            "This reading context's observable control state.",
            None,
        )
    })
    .collect();
    for method in ["set_auto", "set_skip", "pause", "clear_history"] {
        let symbol = format!("reel::reading::{method}");
        api.native_mut(&symbol).returns_parameter("reading");
        reading.push(Member::method(api.native(&symbol).clone().bound(method)));
    }
    api.types.push(NamedType::new(
        "reel::types::Reading",
        "A reading context's observable controls and receiver-based operations.",
        reading,
    ));
    for (function, ty) in [
        ("dialogue", "Dialogue"),
        ("choice", "Choice"),
        ("reading", "Reading"),
    ] {
        api.refine(
            &format!("reel::game::{function}"),
            named(&format!("reel::types::{ty}")),
            &[],
        );
    }
    api.refine("reel::game::history", TypeHint::Array, &[]);
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum DialogueField {
    Lines,
    CurrentLineIds,
    SpeakerName,
    Text,
    Reveal,
    Active,
    Read,
    SpeakerId,
    ContentId,
    Ready,
    AdvanceRequested,
}
#[derive(Clone, Copy)]
#[repr(u32)]
enum LineField {
    Id,
    SpeakerName,
    Text,
    Reveal,
    SpeakerId,
    ContentId,
    Ready,
    Read,
    AdvanceRequested,
}
#[derive(Clone, Copy)]
#[repr(u32)]
enum ChoiceField {
    Options,
    Active,
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum ReadingField {
    Auto,
    Paused,
    Delay,
    Skipping,
    SkipUnread,
    SkipAfterChoices,
    AutoReady,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Narrative {
    dialogue: Value,
    choice: Value,
    interaction: Option<InteractionId>,
    text_revision: u64,
    display_cleared: bool,
    pub(in crate::aside) page: crate::reading::checkpoint::DisplayPage,
    #[serde(skip)]
    option_ids: Vec<(HeapHandle, ChoiceId)>,
    roots: Value,
    root: RootToken,
    reading: Value,
    auto_condition: Value,
    #[serde(skip)]
    reading_values: Option<[Value; 7]>,
    #[cfg(test)]
    #[serde(skip)]
    pub(super) reading_publishes: usize,
    history: Option<history::History>,
}

use super::observable::{array, publish, record};

pub(super) static AUTO_CONDITION: aside_vm::HostPropertyValidator =
    aside_vm::HostPropertyValidator::new("reel.auto-condition", 1, auto_condition_validator);

pub(super) fn auto_condition_validator(
    _: &aside_vm::PropertyRead<'_>,
    _: Value,
    _: u32,
    value: Value,
) -> Result<(), HeapError> {
    value.as_bool().map(|_| ()).ok_or(HeapError::TypeMismatch)
}

fn clear_collection(heap: &mut Heap, host: Value, field: u32) -> Result<(), AdapterError> {
    let current = heap.host_property(PropertyId { host, field })?;
    if heap.sequence_len(current)? != 0 {
        let empty = array(heap)?;
        publish(heap, host, field, empty)?;
    }
    Ok(())
}

impl Narrative {
    #[cfg(test)]
    pub(super) fn condition_weak(&self, heap: &Heap) -> Result<aside_vm::WeakHandle, HeapError> {
        let (source, _) = heap.property_source(PropertyId {
            host: self.auto_condition,
            field: 0,
        })?;
        heap.downgrade(heap.derived_closure(source.host)?)
    }

    fn condition(&self, heap: &Heap) -> Result<bool, AdapterError> {
        heap.host_property(PropertyId {
            host: self.auto_condition,
            field: 0,
        })?
        .as_bool()
        .ok_or(AdapterError::Input("auto condition requires bool"))
    }
    pub(super) fn snapshot_options(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            self.option_ids
                .iter()
                .map(|option| Record::new(Part::NarrativeOption, option)),
        )
    }
    pub(super) fn restore_option(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        identity: SessionIdentity,
    ) -> Result<(), AdapterError> {
        let (handle, mut id): (HeapHandle, ChoiceId) = record.read()?;
        id.rebase(identity);
        self.option_ids.push((map.handle(handle)?, id));
        Ok(())
    }
    pub(super) fn restore(
        &mut self,
        map: &aside_vm::snapshot::SnapshotMap,
        identity: SessionIdentity,
    ) -> Result<(), aside_vm::snapshot::SnapshotError> {
        self.dialogue = map.value(self.dialogue)?;
        self.choice = map.value(self.choice)?;
        self.roots = map.value(self.roots)?;
        self.root = map.root(self.root)?;
        self.reading = map.value(self.reading)?;
        self.auto_condition = map.value(self.auto_condition)?;
        if let Some(id) = &mut self.interaction {
            id.rebase(identity);
        }
        for line in &mut self.page.lines {
            line.id.interaction_id.rebase(identity);
        }
        for (handle, id) in &mut self.option_ids {
            *handle = map.handle(*handle)?;
            id.rebase(identity);
        }
        if let Some(history) = &mut self.history {
            history.value = map.value(history.value)?;
        }
        Ok(())
    }
    pub(in crate::aside) fn new(
        vm: &mut Vm,
        owner: RootOwner,
        types: &super::source::HostTypes,
    ) -> Result<Self, AdapterError> {
        let method = |name: &str| {
            vm.program()
                .package()
                .functions
                .iter()
                .enumerate()
                .find_map(|(i, f)| match f.kind {
                    FunctionKind::Native(n)
                        if vm.program().package().symbols
                            [vm.program().package().natives[n.index()].symbol.index()]
                            == name =>
                    {
                        Some(FunctionId(i as u32))
                    }
                    _ => None,
                })
                .ok_or(AdapterError::Input("narrative callable missing"))
        };
        let clear = method("reel::dialogue::clear")?;
        let select = method("reel::choice::select")?;
        let heap = vm.heap_mut();
        let lines = array(heap)?;
        let ids = array(heap)?;
        let options = array(heap)?;
        let active = heap.alloc_array(&[])?;
        let empty = heap.alloc_string("")?;
        let dialogue = record(
            heap,
            &[
                ("lines", lines),
                ("current_line_ids", ids),
                ("speaker_name", empty),
                ("text", empty),
                ("reveal", Value::int(0)),
                ("active", Value::bool(false)),
                ("read", Value::bool(false)),
                ("speaker_id", Value::NULL),
                ("content_id", Value::NULL),
                ("ready", Value::bool(false)),
                ("advance_requested", Value::bool(false)),
            ],
            &[active],
        )?;
        let choice = record(
            heap,
            &[("options", options), ("active", Value::bool(false))],
            &[],
        )?;
        let reading = record(
            heap,
            &[
                ("auto", Value::bool(false)),
                ("paused", Value::bool(false)),
                ("delay", Value::float(2.)),
                ("skipping", Value::bool(false)),
                ("skip_unread", Value::bool(false)),
                ("skip_after_choices", Value::bool(false)),
                ("auto_ready", Value::bool(true)),
            ],
            &[],
        )?;
        let fields = heap.alloc_table()?;
        let key = heap.alloc_string("condition")?;
        heap.table_store(fields, 0, key, Value::bool(true))?;
        let auto_condition = heap.alloc_host(fields, &[], Some(&AUTO_CONDITION))?;
        heap.observe_host_properties(auto_condition, 1, 0)?;
        let roots = heap.alloc_array(&[dialogue, choice, reading, auto_condition])?;
        for (value, ty) in [
            (dialogue, "reel::types::Dialogue"),
            (choice, "reel::types::Choice"),
            (reading, "reel::types::Reading"),
        ] {
            types.mark(heap, value, ty)?;
        }
        for (object, name, function) in [(dialogue, "clear", clear), (choice, "select", select)] {
            let method = vm.bind_native(object, function)?;
            let heap = vm.heap_mut();
            let fields = heap.host_fields(object)?;
            let key = heap.alloc_string(name)?;
            heap.table_store(fields, heap.table_len(fields)?, key, method)?;
        }
        let root = vm.heap_mut().add_root(owner, RootKind::Host, roots)?;
        Ok(Self {
            dialogue,
            choice,
            interaction: None,
            text_revision: 0,
            display_cleared: false,
            page: Default::default(),
            option_ids: Vec::new(),
            roots,
            root,
            reading,
            auto_condition,
            reading_values: None,
            #[cfg(test)]
            reading_publishes: 0,
            history: None,
        })
    }

    fn sync_reading(&mut self, heap: &mut Heap, session: &Session) -> Result<(), AdapterError> {
        let reading = session.reading();
        let fields = [
            (ReadingField::Auto, Value::bool(reading.automatic())),
            (ReadingField::AutoReady, Value::bool(reading.auto_ready())),
            (ReadingField::Paused, Value::bool(reading.paused())),
            (ReadingField::Skipping, Value::bool(reading.skipping())),
            (ReadingField::SkipUnread, Value::bool(reading.skip_unread())),
            (
                ReadingField::SkipAfterChoices,
                Value::bool(reading.skip_after_choices()),
            ),
            (
                ReadingField::Delay,
                Value::float(reading.delay().as_secs_f64()),
            ),
        ];
        let values = fields.map(|(_, value)| value);
        for (index, (field, value)) in fields.into_iter().enumerate() {
            if self
                .reading_values
                .is_none_or(|previous| !previous[index].same_bits(value))
            {
                publish(heap, self.reading, field as u32, value)?;
                #[cfg(test)]
                {
                    self.reading_publishes += 1;
                }
            }
        }
        self.reading_values = Some(values);
        Ok(())
    }

    fn clear_display(&mut self, heap: &mut Heap, session: &Session) -> Result<(), AdapterError> {
        clear_collection(heap, self.dialogue, DialogueField::Lines as u32)?;
        clear_collection(heap, self.dialogue, DialogueField::CurrentLineIds as u32)?;
        if !matches!(
            session.view().interaction,
            Some(InteractionView::Dialogue(_))
        ) {
            let rows = heap.alloc_array(&[])?;
            heap.set_host_payload(self.dialogue, 0, rows)?;
        }
        let empty = heap.alloc_string("")?;
        for (field, value) in [
            (DialogueField::SpeakerName, empty),
            (DialogueField::Text, empty),
            (DialogueField::SpeakerId, Value::NULL),
            (DialogueField::ContentId, Value::NULL),
            (DialogueField::Reveal, Value::int(0)),
            (DialogueField::Read, Value::bool(false)),
            (DialogueField::Ready, Value::bool(false)),
            (DialogueField::AdvanceRequested, Value::bool(false)),
        ] {
            publish(heap, self.dialogue, field as u32, value)?;
        }
        self.display_cleared = true;
        self.page.lines.clear();
        self.page.cleared = true;
        Ok(())
    }

    pub(super) fn sync(
        &mut self,
        heap: &mut Heap,
        session: &Session,
        chunk: usize,
    ) -> Result<(), AdapterError> {
        let active = session.view().interaction;
        let interaction = active.map(|view| view.id());
        let text_revision = match active {
            Some(InteractionView::Dialogue(d)) => d.text_revision,
            _ => 0,
        };
        let update_text = self.interaction == interaction && self.text_revision != text_revision;
        if interaction != self.interaction {
            if self.interaction.is_some() {
                for line in &mut self.page.lines {
                    if Some(line.id.interaction_id) == self.interaction {
                        line.revealed_count = line.grapheme_boundaries.len() - 1;
                        line.skip_timing();
                    }
                }
                // Completion can remove the active Session row before its final reveal is projected.
                let rows = heap.host_payload(self.dialogue, 0)?;
                for i in 0..heap.sequence_len(rows)? {
                    let row = heap.sequence_get(rows, i)?;
                    let text = heap.host_property(PropertyId {
                        host: row,
                        field: LineField::Text as u32,
                    })?;
                    publish(
                        heap,
                        row,
                        LineField::Reveal as u32,
                        Value::int(super::rich_text::len(heap, text)? as i64),
                    )?;
                    publish(heap, row, LineField::Ready as u32, Value::bool(true))?;
                }
            }
            publish(
                heap,
                self.dialogue,
                DialogueField::Active as u32,
                Value::bool(matches!(active, Some(InteractionView::Dialogue(_)))),
            )?;
            publish(
                heap,
                self.choice,
                ChoiceField::Active as u32,
                Value::bool(matches!(active, Some(InteractionView::Choice(_)))),
            )?;
            self.option_ids.clear();
            if !matches!(active, Some(InteractionView::Dialogue(_))) {
                clear_collection(heap, self.dialogue, DialogueField::CurrentLineIds as u32)?;
            }
            match active {
                Some(InteractionView::Dialogue(dialogue)) => {
                    self.display_cleared = false;
                    self.page.cleared = false;
                    self.page.lines.extend(dialogue.lines().iter().cloned());
                    let rows = heap.alloc_array(&[])?;
                    let ids = array(heap)?;
                    for line in dialogue.lines() {
                        let id = history::line_id(heap, line.id())?;
                        let speaker = line
                            .speaker_id()
                            .map_or(Ok(Value::NULL), |id| text::alloc_string(heap, id, chunk))?;
                        let content = text::alloc_string(heap, line.content_id(), chunk)?;
                        let name = text::alloc_string(heap, line.speaker_name(), chunk)?;
                        let text = super::rich_text::alloc(heap, line.content().styled(), chunk)?;
                        let row = record(
                            heap,
                            &[
                                ("id", id),
                                ("speaker_name", name),
                                ("text", text),
                                ("reveal", Value::int(line.reveal().visible_bytes.end as i64)),
                                ("speaker_id", speaker),
                                ("content_id", content),
                                ("ready", Value::bool(line.playback_complete())),
                                ("read", Value::bool(line.previously_read())),
                                ("advance_requested", Value::bool(line.advance_requested())),
                            ],
                            &[],
                        )?;
                        heap.array_push(rows, row)?;
                        heap.array_push(
                            ids,
                            heap.host_property(PropertyId {
                                host: row,
                                field: LineField::Id as u32,
                            })?,
                        )?;
                    }
                    let lines = heap.host_property(PropertyId {
                        host: self.dialogue,
                        field: DialogueField::Lines as u32,
                    })?;
                    for i in 0..heap.sequence_len(rows)? {
                        heap.array_push(lines, heap.sequence_get(rows, i)?)?;
                    }
                    heap.set_host_payload(self.dialogue, 0, rows)?;
                    publish(
                        heap,
                        self.dialogue,
                        DialogueField::CurrentLineIds as u32,
                        ids,
                    )?;
                    if heap.sequence_len(rows)? != 0 {
                        let first = heap.sequence_get(rows, 0)?;
                        for (target, source) in [
                            (DialogueField::SpeakerName, LineField::SpeakerName),
                            (DialogueField::Text, LineField::Text),
                            (DialogueField::SpeakerId, LineField::SpeakerId),
                            (DialogueField::ContentId, LineField::ContentId),
                        ] {
                            publish(
                                heap,
                                self.dialogue,
                                target as u32,
                                heap.host_property(PropertyId {
                                    host: first,
                                    field: source as u32,
                                })?,
                            )?;
                        }
                    } else {
                        let empty = heap.alloc_string("")?;
                        for (field, value) in [
                            (DialogueField::SpeakerId, Value::NULL),
                            (DialogueField::ContentId, Value::NULL),
                            (DialogueField::SpeakerName, empty),
                            (DialogueField::Text, empty),
                        ] {
                            publish(heap, self.dialogue, field as u32, value)?;
                        }
                    }
                    publish(
                        heap,
                        self.dialogue,
                        DialogueField::Read as u32,
                        Value::bool(dialogue.previously_read()),
                    )?;
                    clear_collection(heap, self.choice, ChoiceField::Options as u32)?;
                }
                Some(InteractionView::Choice(choice)) => {
                    let options = array(heap)?;
                    for option in choice.options() {
                        let id = record(heap, &[], &[])?;
                        let key = text::alloc_string(heap, option.key(), chunk)?;
                        let label = text::alloc_string(heap, option.label(), chunk)?;
                        let row = record(heap, &[("id", id), ("key", key), ("label", label)], &[])?;
                        heap.array_push(options, row)?;
                        self.option_ids
                            .push((heap.handle(id)?, option.id().clone()));
                    }
                    publish(heap, self.choice, ChoiceField::Options as u32, options)?;
                }
                None => {
                    clear_collection(heap, self.choice, ChoiceField::Options as u32)?;
                    let text = heap.host_property(PropertyId {
                        host: self.dialogue,
                        field: DialogueField::Text as u32,
                    })?;
                    publish(
                        heap,
                        self.dialogue,
                        DialogueField::Reveal as u32,
                        Value::int(super::rich_text::len(heap, text)? as i64),
                    )?;
                }
            }
            self.interaction = interaction;
            if self.display_cleared && !matches!(active, Some(InteractionView::Dialogue(_))) {
                let rows = heap.alloc_array(&[])?;
                heap.set_host_payload(self.dialogue, 0, rows)?;
            }
        }
        let mut ready = false;
        if let Some(InteractionView::Dialogue(dialogue)) = active {
            ready = true;
            let rows = heap.host_payload(self.dialogue, 0)?;
            if self.display_cleared && update_text {
                self.page.lines.extend(dialogue.lines().iter().cloned());
                self.page.cleared = false;
                let lines = heap.host_property(PropertyId {
                    host: self.dialogue,
                    field: DialogueField::Lines as u32,
                })?;
                let ids = array(heap)?;
                for index in 0..heap.sequence_len(rows)? {
                    let row = heap.sequence_get(rows, index)?;
                    heap.array_push(lines, row)?;
                    heap.array_push(
                        ids,
                        heap.host_property(PropertyId {
                            host: row,
                            field: LineField::Id as u32,
                        })?,
                    )?;
                }
                publish(
                    heap,
                    self.dialogue,
                    DialogueField::CurrentLineIds as u32,
                    ids,
                )?;
                let first = heap.sequence_get(rows, 0)?;
                for (target, source) in [
                    (DialogueField::SpeakerName, LineField::SpeakerName),
                    (DialogueField::SpeakerId, LineField::SpeakerId),
                    (DialogueField::ContentId, LineField::ContentId),
                ] {
                    publish(
                        heap,
                        self.dialogue,
                        target as u32,
                        heap.host_property(PropertyId {
                            host: first,
                            field: source as u32,
                        })?,
                    )?;
                }
                self.display_cleared = false;
            }
            let page_start = self.page.lines.len().saturating_sub(dialogue.lines().len());
            for (i, line) in dialogue.lines().iter().enumerate() {
                if let Some(saved) = self
                    .page
                    .lines
                    .get_mut(page_start + i)
                    .filter(|saved| saved.id == line.id)
                {
                    saved.revealed_count = line.revealed_count;
                    saved.advance_requested = line.advance_requested;
                    saved.timing = line.timing.clone();
                    if update_text {
                        saved.content = line.content.clone();
                        saved.grapheme_boundaries = line.grapheme_boundaries.clone();
                    }
                }
                let complete = line.playback_complete();
                ready &= complete;
                let row = heap.sequence_get(rows, i as u32)?;
                if update_text {
                    let value = super::rich_text::alloc(heap, line.content().styled(), chunk)?;
                    publish(heap, row, LineField::Text as u32, value)?;
                    if i == 0 {
                        publish(heap, self.dialogue, DialogueField::Text as u32, value)?;
                    }
                }
                publish(heap, row, LineField::Ready as u32, Value::bool(complete))?;
                publish(
                    heap,
                    row,
                    LineField::AdvanceRequested as u32,
                    Value::bool(line.advance_requested()),
                )?;
                publish(
                    heap,
                    row,
                    LineField::Read as u32,
                    Value::bool(line.previously_read()),
                )?;
                publish(
                    heap,
                    row,
                    LineField::Reveal as u32,
                    Value::int(line.reveal().visible_bytes.end as i64),
                )?;
            }
            if !self.display_cleared {
                publish(
                    heap,
                    self.dialogue,
                    DialogueField::Read as u32,
                    Value::bool(dialogue.previously_read()),
                )?;
                publish(
                    heap,
                    self.dialogue,
                    DialogueField::Reveal as u32,
                    Value::int(dialogue.reveal().visible_bytes.end as i64),
                )?;
            }
        }
        publish(
            heap,
            self.dialogue,
            DialogueField::Ready as u32,
            Value::bool(ready && !self.display_cleared),
        )?;
        publish(
            heap,
            self.dialogue,
            DialogueField::AdvanceRequested as u32,
            Value::bool(
                matches!(active, Some(InteractionView::Dialogue(d)) if d.advance_requested()),
            ),
        )?;
        self.sync_reading(heap, session)?;
        if let Some(history) = &mut self.history {
            history.sync(
                heap,
                session,
                self.interaction,
                self.dialogue,
                self.roots,
                chunk,
            )?;
            if update_text && let Some(InteractionView::Dialogue(d)) = active {
                history.update_dialogue(heap, session, d, self.dialogue, chunk)?;
            }
        }
        self.text_revision = text_revision;
        Ok(())
    }
}

impl SessionVm {
    pub(crate) fn checkpoint_display(&self) -> Option<&crate::reading::checkpoint::DisplayPage> {
        self.narrative.as_ref().map(|model| &model.page)
    }

    pub(super) fn sync_auto_condition(
        &mut self,
        session: &mut Session,
    ) -> Result<(), AdapterError> {
        if let Some(model) = &self.narrative {
            session.set_auto_condition(model.condition(self.vm.heap())?);
        }
        self.sync_context_conditions(session)?;
        Ok(())
    }

    /// Publish reading controls and accepted narrative input at the stable input boundary.
    pub fn sync_reading(&mut self, session: &Session) -> Result<(), AdapterError> {
        self.same_session(session)?;
        if self.reactive_ready && self.derived_task.is_none() && self.narrative.is_some() {
            self.sync_narrative(session)?;
        }
        Ok(())
    }

    pub(super) fn sync_narrative(&mut self, session: &Session) -> Result<(), AdapterError> {
        if let Some(model) = &mut self.narrative {
            model.sync(self.vm.heap_mut(), session, self.read_limit)?;
        }
        Ok(())
    }
    pub(super) fn dispatch_narrative(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        let receiver = symbol.starts_with("reel::reading::")
            || matches!(symbol, "reel::dialogue::clear" | "reel::choice::select");
        let id = if receiver {
            self.narrative_id(session, self.vm.host_arguments(ticket)?[0])?
        } else {
            self.observe_reading_selection(session)?;
            session.reading_id
        };
        let initialize = id == session.reading_id && self.narrative.is_none();
        if initialize {
            self.narrative = Some(Narrative::new(
                &mut self.vm,
                self.root_owner,
                &self.stage.types,
            )?);
        }
        let sync = initialize || !self.vm.heap().reactive_evaluating();
        let foreground = id == session.reading_id;
        let mut model = self.take_narrative(session, id).unwrap();
        let result = session.with_reading(id, |session| {
            if sync {
                model.sync(self.vm.heap_mut(), session, self.read_limit)?;
            }
            self.dispatch_narrative_model(session, ticket, symbol, &mut model, sync, foreground)
        })?;
        self.put_narrative(session, id, model);
        result?;
        if symbol == "reel::game::clear_read_progress" {
            for frame in &mut self.narrative_contexts.frames {
                if let Some(model) = &mut frame.model {
                    session.with_reading(frame.parent, |session| {
                        model.sync(self.vm.heap_mut(), session, self.read_limit)
                    })??;
                }
            }
        }
        if sync {
            self.sync_all_reading_controls(session)?;
        }
        Ok(())
    }

    fn dispatch_narrative_model(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        symbol: &str,
        model: &mut Narrative,
        sync: bool,
        foreground: bool,
    ) -> Result<(), AdapterError> {
        let value = match symbol {
            "reel::game::dialogue" => model.dialogue,
            "reel::game::choice" => model.choice,
            "reel::game::reading" => model.reading,
            "reel::reading::clear_history" => {
                let keep = self.vm.host_arguments(ticket)?[1]
                    .as_int()
                    .filter(|&value| value >= 0)
                    .ok_or(AdapterError::Input(
                        "history keep_last requires a nonnegative int",
                    ))?;
                session.clear_history(usize::try_from(keep).unwrap_or(usize::MAX));
                model.reading
            }
            "reel::game::clear_read_progress" => {
                session.clear_read_progress()?;
                self.activities.set_skipping(session.reading().skipping());
                if !session.reading().skipping() {
                    self.skip_input = None;
                }
                Value::NULL
            }
            "reel::game::history" => {
                if model.history.is_none() {
                    let history = history::History::new(self.vm.heap_mut())?;
                    self.vm.heap_mut().array_push(model.roots, history.value)?;
                    model.history = Some(history);
                }
                model.history.as_ref().unwrap().value
            }
            "reel::reading::set_auto" => {
                let args = self.vm.host_arguments(ticket)?;
                let enabled = if args[1].tag() == ValueTag::Null {
                    session.reading().automatic()
                } else {
                    args[1]
                        .as_bool()
                        .ok_or(AdapterError::Input("auto requires bool or null"))?
                };
                let delay = if args[2].tag() == ValueTag::Null {
                    None
                } else {
                    let seconds = args[2]
                        .as_float()
                        .or_else(|| args[2].as_int().map(|n| n as f64))
                        .ok_or(AdapterError::Input("auto delay requires seconds"))?;
                    Some(
                        std::time::Duration::try_from_secs_f64(seconds).map_err(|_| {
                            AdapterError::Input(
                                "auto delay must be finite, nonnegative and representable",
                            )
                        })?,
                    )
                };
                let condition = args[3];
                if condition.tag() != ValueTag::Null {
                    let heap = self.vm.heap_mut();
                    let property = PropertyId {
                        host: model.auto_condition,
                        field: 0,
                    };
                    if heap.is_property_ref(condition) {
                        heap.write_host_property(property, condition, true)?;
                    } else {
                        auto_condition_validator(
                            &aside_vm::PropertyRead::current(heap),
                            model.auto_condition,
                            0,
                            condition,
                        )?;
                        let reference = heap.alloc_property_ref(property, true)?;
                        heap.unbind_property(reference)?;
                        heap.write_host_property(property, condition, true)?;
                    }
                }
                session.set_auto_reading(enabled, delay);
                session.set_auto_condition(model.condition(self.vm.heap())?);
                model.reading
            }
            "reel::reading::set_skip" => {
                let args = self.vm.host_arguments(ticket)?;
                let enabled = args[1]
                    .as_bool()
                    .ok_or(AdapterError::Input("skip requires bool"))?;
                let unread = args[2]
                    .as_bool()
                    .ok_or(AdapterError::Input("skip unread requires bool"))?;
                let after_choices = args[3]
                    .as_bool()
                    .ok_or(AdapterError::Input("skip after_choices requires bool"))?;
                session.set_skip_reading(enabled, unread, after_choices);
                if foreground {
                    self.skip_input = self
                        .tasks
                        .iter()
                        .find(|callback| callback.task == ticket.task)
                        .and_then(|callback| callback.node)
                        .filter(|node| {
                            session.reading().skipping()
                                && self.stage.scene.node(*node).is_some_and(|node| {
                                    matches!(node.kind, reel_ui::stage::NodeKind::Input { .. })
                                })
                        });
                }
                model.reading
            }
            "reel::reading::pause" => {
                let paused = self.vm.host_arguments(ticket)?[1]
                    .as_bool()
                    .ok_or(AdapterError::Input("reading pause requires bool"))?;
                session.pause_reading(paused);
                model.reading
            }
            "reel::dialogue::clear" => {
                session.clear_dialogue_advance_request();
                model.clear_display(self.vm.heap_mut(), session)?;
                Value::NULL
            }
            "reel::choice::select" => {
                let input = self.vm.host_arguments(ticket)?[1];
                let option = if input.tag() == ValueTag::Object {
                    let handle = self.vm.heap().handle(input)?;
                    model
                        .option_ids
                        .iter()
                        .find(|(id, _)| *id == handle)
                        .map(|(_, option)| option.clone())
                } else {
                    None
                };
                let owner = self
                    .tasks
                    .iter()
                    .find(|callback| callback.task == ticket.task)
                    .map_or(self.owner, |callback| callback.owner);
                Value::bool(option.is_some_and(|id| {
                    session
                        .handle_local_input(owner, InputAction::Select(id))
                        .is_ok()
                }))
            }
            _ => unreachable!(),
        };
        if sync {
            model.sync(self.vm.heap_mut(), session, self.read_limit)?;
        }
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }
}

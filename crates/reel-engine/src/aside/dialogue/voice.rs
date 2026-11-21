use crate::{
    Session,
    aside::{AdapterError, SessionVm, media, text},
    audio::{AudioGroup, AudioRate, AudioRequest},
};
use aside_vm::{Heap, Value, ValueTag, executor::TaskId};
use std::time::Duration;

pub(in crate::aside) fn voice_input(
    heap: &Heap,
    path: Value,
    gain: Value,
    chunk: usize,
) -> Result<Option<AudioRequest>, AdapterError> {
    if path.tag() == ValueTag::Null {
        return Ok(None);
    }
    let source = text::read_string(heap, path, chunk)?;
    if source.is_empty() {
        return Err(AdapterError::Input("voice source must not be empty"));
    }
    Ok(Some(AudioRequest {
        source,
        loop_region: None,
        volume: media::volume(gain)?,
        rate: AudioRate::default(),
        group: AudioGroup::Voice,
        pan: 0.,
        fade_in: Duration::ZERO,
        start_position: Duration::ZERO,
        end_position: None,
    }))
}

impl SessionVm {
    pub(in crate::aside) fn set_dialogue_voice(
        &mut self,
        session: &Session,
        task: TaskId,
        owner: Value,
        request: Option<AudioRequest>,
    ) -> Result<(), AdapterError> {
        let previous = self.dialogue_voice()?;
        let start = request.filter(|_| session.skipping_effects().is_none());
        if start.is_none() && previous.tag() == ValueTag::Null {
            return Ok(());
        }
        self.stop_dialogue_voice(previous)?;
        let value = if let Some(request) = start {
            self.start_dialogue_voice(task, owner, request)?
        } else {
            Value::NULL
        };
        self.publish_dialogue_voice(value)
    }
}

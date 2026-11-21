//! Reading contents move between the foreground and suspended parent contexts.
use super::*;
use crate::Session;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReadingId(pub u64);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ParkedReading {
    pub id: ReadingId,
    pub data: ReadingData,
}

impl Session {
    /// The foreground reading can finish eligible effects between its stopping points.
    pub(crate) fn skipping_effects(&self) -> Option<ReadingId> {
        if !self.reading_data.control.skipping() {
            return None;
        }
        match &self.reading_data.active {
            Some(Interaction::Choice(_)) => None,
            Some(Interaction::Dialogue(line)) if !line.reveal_skippable => None,
            _ => Some(self.reading_id),
        }
    }

    pub(crate) fn prepare_reading(&mut self) -> Result<ReadingId, SessionError> {
        self.ensure_running()?;
        if self.reading_data.completion.is_some()
            || matches!(
                &self.reading_data.active,
                Some(Interaction::Choice(_))
                    | Some(Interaction::Dialogue(crate::DialogueInteraction {
                        awaiting_advance: true,
                        ..
                    }))
            )
        {
            return Err(SessionError::InteractionActive);
        }
        self.parked_readings
            .try_reserve(1)
            .map_err(|_| SessionError::AllocationFailed)?;
        let id = self.next_reading.ok_or(SessionError::IdentityExhausted)?;
        Ok(ReadingId(id))
    }

    /// Commit an entry after host publication and both context stacks are prepared.
    pub(crate) fn push_reading(&mut self, id: ReadingId) {
        let ReadingId(id) = id;
        self.next_reading = id.checked_add(1).filter(|&id| id <= i64::MAX as u64);
        self.reading_data.control.set_skip(false, false, false);
        let mut data = ReadingData::default();
        data.control.activate_after(self.reading_data.control);
        let parent = std::mem::replace(&mut self.reading_data, data);
        self.parked_readings.push(ParkedReading {
            id: self.reading_id,
            data: parent,
        });
        self.reading_id = ReadingId(id);
    }

    pub(crate) fn pop_reading(&mut self) {
        let mut parent = self.parked_readings.pop().unwrap();
        parent
            .data
            .control
            .activate_after(self.reading_data.control);
        self.reading_data = parent.data;
        self.reading_id = parent.id;
    }

    /// Temporarily select a retained context for one synchronous host operation.
    /// No author code or scheduling runs before the foreground is restored.
    pub(crate) fn with_reading<T>(
        &mut self,
        id: ReadingId,
        body: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, SessionError> {
        if self.reading_id == id {
            return Ok(body(self));
        }
        let index = self
            .parked_readings
            .iter()
            .position(|p| p.id == id)
            .ok_or(SessionError::InvalidReadingContext)?;
        std::mem::swap(
            &mut self.reading_data,
            &mut self.parked_readings[index].data,
        );
        let value = body(self);
        std::mem::swap(
            &mut self.reading_data,
            &mut self.parked_readings[index].data,
        );
        Ok(value)
    }

    pub(crate) fn reserve_parent_read_progress(&mut self) -> Result<(), SessionError> {
        let parent = self
            .parked_readings
            .last()
            .ok_or(SessionError::InvalidReadingContext)?;
        if let Some(Interaction::Dialogue(dialogue)) = &parent.data.active {
            self.read_progress
                .ids
                .try_reserve(dialogue.lines.len().max(dialogue.pending_read_ids.len()))
                .map_err(|_| SessionError::AllocationFailed)?;
        }
        Ok(())
    }
}

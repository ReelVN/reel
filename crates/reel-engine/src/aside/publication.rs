use crate::{HistoryGroup, Interaction, InteractionId, Session, SessionError};

/// Private synchronous staging; no VM reentry, observers or callbacks occur
/// while constructing this. Reuse the Session's admission checks and reserved
/// capacity, then restore all visible state before crossing the VM boundary.
pub(super) struct Publication {
    interaction: Interaction,
    history: Option<HistoryGroup>,
    history_bytes: usize,
    next: Option<u64>,
}
impl Publication {
    pub fn stage(
        session: &mut Session,
        prepare: impl FnOnce(&mut Session) -> Result<InteractionId, SessionError>,
    ) -> Result<Self, SessionError> {
        let history_len = session.history.len();
        let old_bytes = session.history_text_bytes;
        let old_next = session.next_interaction;
        prepare(session)?;
        let publication = Self {
            interaction: session.active.take().expect("successful admission"),
            history: (session.history.len() > history_len).then(|| session.history.pop().unwrap()),
            history_bytes: session.history_text_bytes,
            next: session.next_interaction,
        };
        session.history_text_bytes = old_bytes;
        session.next_interaction = old_next;
        Ok(publication)
    }
    pub fn id(&self) -> InteractionId {
        self.interaction.id()
    }
    pub fn commit(self, session: &mut Session) {
        session.active = Some(self.interaction);
        if let Some(history) = self.history {
            session.history.push(history);
        }
        session.history_text_bytes = self.history_bytes;
        session.next_interaction = self.next;
    }
}

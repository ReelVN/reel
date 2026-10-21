use crate::{HistoryGroup, Interaction, InteractionId, Session, SessionError};

/// Private synchronous staging; no VM reentry, observers or callbacks occur
/// while constructing this. Reuse the Session's admission checks and reserved
/// capacity, then restore all visible state before crossing the VM boundary.
pub(super) struct Publication {
    interaction: Interaction,
    history: Option<HistoryGroup>,
    next: Option<u64>,
}
impl Publication {
    pub fn stage(
        session: &mut Session,
        prepare: impl FnOnce(&mut Session) -> Result<InteractionId, SessionError>,
    ) -> Result<Self, SessionError> {
        let history_len = session.history.len();
        let old_next = session.next_interaction;
        let previous = if matches!(&session.active, Some(Interaction::Dialogue(d)) if d.open && !d.awaiting_advance)
        {
            session.active.take()
        } else {
            None
        };
        if let Err(error) = prepare(session) {
            if previous.is_some() {
                session.active = previous;
            }
            return Err(error);
        }
        let publication = Self {
            interaction: session.active.take().expect("successful admission"),
            history: (session.history.len() > history_len).then(|| session.history.pop().unwrap()),
            next: session.next_interaction,
        };
        session.next_interaction = old_next;
        session.active = previous;
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
        session.next_interaction = self.next;
    }
}

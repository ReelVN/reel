//! Optional, bounded dialogue checkpoints for the current reading timeline.
use super::*;
use reel_engine::{
    InteractionView, Session, SessionIdentity,
    aside::checkpoint::{CapturedCheckpoint, CheckpointRecord},
};
use std::collections::VecDeque;

const MAX_POINTS: usize = 64;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct HistoryWindow {
    identity: Option<SessionIdentity>,
    attempted: Option<u64>,
    records: VecDeque<(u64, u64, CapturedCheckpoint)>,
    bytes: u64,
}

impl HistoryWindow {
    pub(super) fn capture(&mut self, driver: &mut AsideSessionUi, group: u64) {
        let identity = driver.ui().session().identity();
        if self.identity != Some(identity) {
            self.reset();
            self.identity = Some(identity);
        }
        if self.attempted == Some(group) {
            return;
        }
        self.attempted = Some(group);
        let Some(checkpoint) = driver.capture_history() else {
            return;
        };
        let bytes = checkpoint.record().retained_bytes();
        if bytes > MAX_BYTES {
            return;
        }
        while self.records.len() >= MAX_POINTS || self.bytes + bytes > MAX_BYTES {
            let (_, removed, _) = self.records.pop_front().unwrap();
            self.bytes -= removed;
        }
        self.bytes += bytes;
        self.records.push_back((group, bytes, checkpoint));
        self.publish(driver);
    }

    pub(super) fn record(&self, session: &Session, group: u64) -> Option<CheckpointRecord> {
        if self.identity != Some(session.identity()) {
            return None;
        }
        self.records
            .iter()
            .find(|entry| entry.0 == group)?
            .2
            .record()
            .history_jump(session, group)
    }

    pub(super) fn adopt(&mut self, driver: &mut AsideSessionUi, group: u64) {
        while self.records.back().is_some_and(|entry| entry.0 > group) {
            let (_, removed, _) = self.records.pop_back().unwrap();
            self.bytes -= removed;
        }
        self.attempted = Some(group);
        self.identity = Some(driver.ui().session().identity());
        self.publish(driver);
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    fn publish(&self, driver: &mut AsideSessionUi) {
        driver
            .adapter_mut()
            .set_history_points(self.records.iter().map(|entry| entry.0));
    }
}

impl StoryRuntime {
    pub(super) fn remember_dialogue(&mut self, receipt: &crate::session::RecordedProjection) {
        let Some(saves) = self.saves.as_mut() else {
            return;
        };
        let driver = self.driver.as_mut().unwrap();
        if !driver.adapter().tracks_history() {
            if saves.history.identity.is_some() {
                saves.history.reset();
            }
            return;
        }
        if self.closing
            || self.collecting
            || self.starting.is_some()
            || saves.busy()
            || !receipt.current
            || !driver.adapter().stage_ready()
        {
            return;
        }
        let Some(interaction) = receipt.completed_dialogue() else {
            return;
        };
        let view = driver.ui().session().view();
        if matches!(view.interaction, Some(InteractionView::Dialogue(dialogue)) if dialogue.id() == interaction && dialogue.awaiting_advance())
            && view
                .history
                .last()
                .is_some_and(|group| group.interaction_id() == interaction)
        {
            saves.history.capture(driver, interaction.generation());
        }
    }
}

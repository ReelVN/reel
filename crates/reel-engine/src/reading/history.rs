//! The active transcript shares completed pages with detached checkpoints.
use crate::{HistoryGroup, HistoryLimits, HistoryLine};
use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::VecDeque, ops::Index, sync::Arc};

const PAGE_ROWS: usize = 32;

#[derive(Debug, Default)]
struct Page {
    rows: Vec<HistoryGroup>,
    payload_bytes: usize,
    line_count: usize,
}

impl Clone for Page {
    fn clone(&self) -> Self {
        let rows = self.rows.clone();
        let payload_bytes = rows.iter().map(group_bytes).sum();
        let line_count = rows.iter().map(|group| group.lines.len().max(1)).sum();
        Self {
            rows,
            payload_bytes,
            line_count,
        }
    }
}

/// Recent history groups in reading order. Sessions default to 200 lines and
/// 1 MiB of logical storage, trimming older rows in batches according to their
/// current policy. A newest line larger than that allowance remains readable.
/// Checkpoints share retained pages.
#[derive(Debug, Clone, Default)]
pub struct HistoryLog {
    pages: VecDeque<Arc<Page>>,
    len: usize,
}

impl HistoryLog {
    /// Bound each reading timeline, retaining original identities and the newest
    /// line even when that line alone exceeds the ordinary history allowance.
    /// Leave space for another batch before rebuilding the observable prefix.
    pub(crate) fn enforce_limits(&mut self, limits: HistoryLimits) {
        if limits.max_lines == 0 {
            *self = Self::default();
            return;
        }
        let count: usize = self.pages.iter().map(|page| page.line_count).sum();
        let bytes = self.retained_bytes();
        if count <= limits.max_lines && bytes <= limits.max_bytes {
            return;
        }
        let keep = if count > limits.max_lines {
            limits.max_lines - limits.max_lines / 8
        } else {
            count
        };
        let byte_limit = if bytes > limits.max_bytes {
            limits.max_bytes - limits.max_bytes / 8
        } else {
            bytes
        };
        let mut retained_lines = 0;
        let mut retained_bytes = 0;
        let mut first_group = self.len;
        let mut first_line = 0;
        'groups: for index in (0..self.len).rev() {
            let group = &self[index];
            let overhead = size_of::<HistoryGroup>() + group.page_locale.capacity();
            let mut first = true;
            for line in (0..group.lines.len().max(1)).rev() {
                let added = if group.lines.is_empty() {
                    0
                } else {
                    size_of::<HistoryLine>() + line_bytes(&group.lines[line])
                } + if first { overhead } else { 0 };
                if retained_lines != 0
                    && (retained_lines == keep || retained_bytes + added > byte_limit)
                {
                    break 'groups;
                }
                retained_lines += 1;
                retained_bytes += added;
                first_group = index;
                first_line = line;
                first = false;
            }
        }
        if first_line != 0 {
            self.edit(first_group, |group| {
                group.lines.drain(..first_line);
                group.lines.shrink_to_fit();
            });
        }
        self.retain_from(first_group);
    }

    /// Number of groups in the transcript.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the transcript has any groups.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// All groups in reading order.
    pub fn iter(&self) -> impl Iterator<Item = &HistoryGroup> {
        self.pages.iter().flat_map(|page| page.rows.iter())
    }

    pub(crate) fn voice_paths(&self) -> impl Iterator<Item = (u64, usize, &str)> {
        self.iter().flat_map(|group| {
            group.lines().iter().filter_map(|line| {
                let id = line.id();
                Some((id.interaction_id().generation(), id.index(), line.voice()?))
            })
        })
    }

    pub(crate) fn restore_voice(&mut self, group: u64, index: usize, path: &str) -> bool {
        let Some(row) = self.find_interaction(group) else {
            return false;
        };
        let Some(line) = self[row]
            .lines()
            .iter()
            .position(|line| line.id().index() == index && line.voice().is_none())
        else {
            return false;
        };
        self.edit(row, |group| group.lines[line].voice = Some(Arc::from(path)));
        true
    }

    /// Read one group by its zero-based position.
    pub fn get(&self, index: usize) -> Option<&HistoryGroup> {
        let (page, row) = self.locate(index)?;
        Some(&self.pages[page].rows[row])
    }

    /// Read the most recent group.
    pub fn last(&self) -> Option<&HistoryGroup> {
        self.get(self.len.checked_sub(1)?)
    }

    /// Read the earliest group.
    pub fn first(&self) -> Option<&HistoryGroup> {
        self.get(0)
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut HistoryGroup> {
        self.pages
            .iter_mut()
            .flat_map(|page| Arc::make_mut(page).rows.iter_mut())
    }

    fn locate(&self, index: usize) -> Option<(usize, usize)> {
        if index >= self.len {
            return None;
        }
        let first = self.pages.front()?.rows.len();
        if index < first {
            Some((0, index))
        } else {
            let tail = index - first;
            Some((1 + tail / PAGE_ROWS, tail % PAGE_ROWS))
        }
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut HistoryGroup> {
        let (page, row) = self.locate(index)?;
        Some(&mut Arc::make_mut(&mut self.pages[page]).rows[row])
    }

    pub(crate) fn edit(&mut self, index: usize, edit: impl FnOnce(&mut HistoryGroup)) {
        let (page, row) = self.locate(index).unwrap();
        let page = Arc::make_mut(&mut self.pages[page]);
        page.payload_bytes -= group_bytes(&page.rows[row]);
        page.line_count -= page.rows[row].lines.len().max(1);
        edit(&mut page.rows[row]);
        page.payload_bytes += group_bytes(&page.rows[row]);
        page.line_count += page.rows[row].lines.len().max(1);
    }

    pub(crate) fn find_interaction(&self, generation: u64) -> Option<usize> {
        let mut low = 0;
        let mut high = self.len;
        while low < high {
            let mid = low + (high - low) / 2;
            match self[mid].interaction_id.generation.cmp(&generation) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    pub(crate) fn reserve_append(&mut self) -> Result<(), std::collections::TryReserveError> {
        if self
            .pages
            .back()
            .is_none_or(|page| page.rows.len() == PAGE_ROWS)
        {
            let mut page = Page::default();
            page.rows.try_reserve_exact(PAGE_ROWS)?;
            self.pages.try_reserve(1)?;
            self.pages.push_back(Arc::new(page));
        }
        let tail = Arc::make_mut(self.pages.back_mut().unwrap());
        tail.rows.try_reserve_exact(PAGE_ROWS - tail.rows.len())
    }

    pub(crate) fn push(&mut self, group: HistoryGroup) {
        let tail = Arc::make_mut(self.pages.back_mut().unwrap());
        tail.payload_bytes += group_bytes(&group);
        tail.line_count += group.lines.len().max(1);
        tail.rows.push(group);
        self.len += 1;
    }

    pub(crate) fn pop(&mut self) -> Option<HistoryGroup> {
        if self.pages.back()?.rows.is_empty() {
            self.pages.pop_back();
        }
        let tail = Arc::make_mut(self.pages.back_mut()?);
        let group = tail.rows.pop()?;
        tail.payload_bytes -= group_bytes(&group);
        tail.line_count -= group.lines.len().max(1);
        self.len -= 1;
        Some(group)
    }

    pub(crate) fn retain_from(&mut self, index: usize) {
        let Some((page, row)) = self.locate(index) else {
            *self = Self::default();
            return;
        };
        self.pages.drain(..page);
        if row > 0 {
            let first = Arc::make_mut(self.pages.front_mut().unwrap());
            first.payload_bytes -= first.rows[..row].iter().map(group_bytes).sum::<usize>();
            first.line_count -= first.rows[..row]
                .iter()
                .map(|group| group.lines.len().max(1))
                .sum::<usize>();
            first.rows.drain(..row);
            first.rows.shrink_to_fit();
        }
        self.pages.shrink_to_fit();
        self.len -= index;
    }

    pub(crate) fn capacity(&self) -> usize {
        self.pages.iter().map(|page| page.rows.capacity()).sum()
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.pages
            .iter()
            .map(|page| page_bytes(page))
            .sum::<usize>()
            + self.pages.capacity() * size_of::<Arc<Page>>()
    }

    pub(crate) fn share_page(&mut self, source: &Self, index: usize) -> Option<usize> {
        let page = source.pages.get(index)?;
        let capacity = self.pages.capacity();
        self.len += page.rows.len();
        self.pages.push_back(Arc::clone(page));
        Some(page_bytes(page) + (self.pages.capacity() - capacity) * size_of::<Arc<Page>>())
    }

    pub(crate) fn prefix_through(&self, generation: u64) -> Option<Self> {
        let index = self.find_interaction(generation)?;
        let (page, row) = self.locate(index)?;
        let mut prefix = Self {
            pages: self.pages.iter().take(page + 1).cloned().collect(),
            len: index + 1,
        };
        let tail = prefix.pages.back_mut().unwrap();
        if row + 1 < tail.rows.len() {
            let tail = Arc::make_mut(tail);
            tail.payload_bytes -= tail.rows[row + 1..].iter().map(group_bytes).sum::<usize>();
            tail.line_count -= tail.rows[row + 1..]
                .iter()
                .map(|group| group.lines.len().max(1))
                .sum::<usize>();
            tail.rows.truncate(row + 1);
        }
        Some(prefix)
    }
}

impl Index<usize> for HistoryLog {
    type Output = HistoryGroup;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("history group index")
    }
}

fn group_bytes(group: &HistoryGroup) -> usize {
    group.page_locale.capacity()
        + group.lines.capacity() * size_of::<HistoryLine>()
        + group.lines.iter().map(line_bytes).sum::<usize>()
}

fn line_bytes(line: &HistoryLine) -> usize {
    line.speaker_id.as_ref().map_or(0, String::capacity)
        + line.speaker_name.capacity()
        + line.content.id.capacity()
        + line.content.fallback.storage_bytes()
        + line
            .voice
            .as_ref()
            .map_or(0, |path| path.len() + 2 * size_of::<usize>())
}

fn page_bytes(page: &Page) -> usize {
    size_of::<Page>() + page.rows.capacity() * size_of::<HistoryGroup>() + page.payload_bytes
}

// The on-disk representation remains the same flat sequence of groups.
impl Serialize for HistoryLog {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for group in self.iter() {
            sequence.serialize_element(group)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for HistoryLog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Groups;
        impl<'de> Visitor<'de> for Groups {
            type Value = HistoryLog;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a sequence of history groups")
            }

            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut sequence: S,
            ) -> Result<Self::Value, S::Error> {
                let mut history = HistoryLog::default();
                while let Some(group) = sequence.next_element()? {
                    history.reserve_append().map_err(serde::de::Error::custom)?;
                    history.push(group);
                }
                Ok(history)
            }
        }
        deserializer.deserialize_seq(Groups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MAX_LINES: usize = 200;
    const MAX_BYTES: usize = 1024 * 1024;
    const TRIM_LINES: usize = MAX_LINES / 8;
    use crate::{
        DialogueLineId, Epoch, HistoryGroupId, InteractionId, Session, SessionId, SessionIdentity,
        TextContent,
    };

    fn line(group: &HistoryGroup, index: usize, text: &str) -> HistoryLine {
        HistoryLine {
            id: DialogueLineId {
                interaction_id: group.interaction_id,
                index,
            },
            speaker_id: None,
            speaker_name: String::new(),
            content: TextContent {
                id: format!("{}:{index}", group.interaction_id.generation),
                fallback: text.into(),
            },
            voice: Some(Arc::from("voice/retained.opus")),
        }
    }

    fn group(generation: u64) -> HistoryGroup {
        let interaction_id = InteractionId {
            identity: SessionIdentity {
                session_id: SessionId(1),
                epoch: Epoch(1),
            },
            generation,
        };
        HistoryGroup {
            group_id: HistoryGroupId(interaction_id),
            interaction_id,
            lines: Vec::new(),
            page_locale: "en".into(),
        }
    }

    #[test]
    fn shared_history_keeps_its_prefix_and_flat_disk_encoding() {
        let original: Vec<_> = (1..=72).map(group).collect();
        let bytes = postcard::to_allocvec(&original).unwrap();
        let mut live: HistoryLog = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(postcard::to_allocvec(&live).unwrap(), bytes);
        live.retain_from(16);
        let captured = live.prefix_through(67).unwrap();
        live.edit(50, |row| row.page_locale = "zh-CN".into());
        live.reserve_append().unwrap();
        live.push(group(73));
        assert_eq!(
            captured.iter().collect::<Vec<_>>(),
            original[16..67].iter().collect::<Vec<_>>()
        );
        assert_eq!(live[50].page_locale, "zh-CN");
        assert_eq!(live.last().unwrap().interaction_id.generation, 73);
    }

    #[test]
    fn growing_history_has_a_bounded_live_tail_and_keeps_snapshot_pages() {
        let mut live = HistoryLog::default();
        let mut captured = None;
        for generation in 1..=10_000 {
            let mut row = group(generation);
            row.lines.push(line(
                &row,
                0,
                "Recorded history with a retained voice path.",
            ));
            live.reserve_append().unwrap();
            live.push(row);
            live.enforce_limits(HistoryLimits::default());
            assert!(live.len() <= MAX_LINES);
            assert!(live.retained_bytes() <= MAX_BYTES);
            if generation == 180 {
                captured = live.prefix_through(180);
            }
        }
        assert!(live.len() >= MAX_LINES - TRIM_LINES);
        assert_eq!(live.last().unwrap().interaction_id.generation, 10_000);
        assert!(live.first().unwrap().interaction_id.generation >= 9_801);
        let captured = captured.unwrap();
        assert_eq!(captured.len(), 180);
        assert_eq!(captured[0].interaction_id.generation, 1);
        assert_eq!(captured.last().unwrap().interaction_id.generation, 180);
        let encoded = postcard::to_allocvec(&live).unwrap();
        let mut restored: HistoryLog = postcard::from_bytes(&encoded).unwrap();
        for (group, index, path) in live.voice_paths() {
            assert!(restored.restore_voice(group, index, path));
        }
        assert_eq!(
            restored.iter().collect::<Vec<_>>(),
            live.iter().collect::<Vec<_>>()
        );
        let full: Vec<_> = (1..=1000)
            .map(|generation| {
                let mut row = group(generation);
                row.lines.push(line(&row, 0, "original archive"));
                row.lines[0].voice = None;
                row
            })
            .collect();
        let old_archive = postcard::to_allocvec(&full).unwrap();
        let mut restored: HistoryLog = postcard::from_bytes(&old_archive).unwrap();
        for generation in 1..=1000 {
            assert!(restored.restore_voice(generation, 0, "voice/original.opus"));
        }
        assert!(!restored.restore_voice(1000, 0, "duplicate.opus"));
        assert!(!restored.restore_voice(1001, 0, "future.opus"));
        restored.enforce_limits(HistoryLimits::default());
        assert!(restored.len() <= MAX_LINES);
    }

    #[test]
    fn large_groups_trim_actual_lines_without_changing_display_or_ids() {
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let inputs = (0..240)
            .map(|_| crate::DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: "same-content",
                text: "A retained NVL row".into(),
                history: Default::default(),
            })
            .collect::<Vec<_>>();
        let id = session
            .begin_say(session.root_owner(), &inputs, "en", true)
            .unwrap();
        let history = session.view().history;
        assert_eq!(history.len(), 1);
        let lines = &history[0].lines;
        assert_eq!(lines.len(), MAX_LINES - TRIM_LINES);
        assert_eq!(lines[0].id.index(), 240 - lines.len());
        assert_eq!(lines[0].id.interaction_id(), id);
        let Some(crate::InteractionView::Dialogue(display)) = session.view().interaction else {
            panic!("dialogue");
        };
        assert_eq!(display.lines().len(), 240);
        session.clear_history(37);
        assert_eq!(session.view().history[0].lines.len(), 37);
        assert_eq!(session.view().history[0].lines[0].id.index(), 203);
    }

    #[test]
    fn byte_limit_releases_large_rows_and_retains_one_oversized_latest_line() {
        let paragraph = "x".repeat(MAX_BYTES / 3);
        let mut groups = Vec::new();
        for generation in 1..=6 {
            let mut row = group(generation);
            row.lines.push(line(&row, 0, &paragraph));
            groups.push(row);
        }
        let encoded = postcard::to_allocvec(&groups).unwrap();
        let mut restored: HistoryLog = postcard::from_bytes(&encoded).unwrap();
        restored.enforce_limits(HistoryLimits::default());
        assert!(restored.len() < groups.len());
        assert!(restored.retained_bytes() <= MAX_BYTES);
        assert_eq!(restored.last().unwrap().interaction_id.generation, 6);
        let mut oversized = group(7);
        oversized
            .lines
            .push(line(&oversized, 0, &"y".repeat(2 * MAX_BYTES)));
        restored.reserve_append().unwrap();
        restored.push(oversized);
        restored.enforce_limits(HistoryLimits::default());
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].lines[0].content.fallback.len(), 2 * MAX_BYTES);
        assert!(restored.retained_bytes() > MAX_BYTES);
    }
}

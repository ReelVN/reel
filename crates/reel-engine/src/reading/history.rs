//! The active transcript shares completed pages with detached checkpoints.
use crate::{HistoryGroup, HistoryLine};
use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::VecDeque, ops::Index, sync::Arc};

const PAGE_ROWS: usize = 32;

#[derive(Debug, Default)]
struct Page {
    rows: Vec<HistoryGroup>,
    payload_bytes: usize,
}

impl Clone for Page {
    fn clone(&self) -> Self {
        let rows = self.rows.clone();
        let payload_bytes = rows.iter().map(group_bytes).sum();
        Self {
            rows,
            payload_bytes,
        }
    }
}

/// History groups in reading order. Checkpoints can retain pages by reference.
#[derive(Debug, Clone, Default)]
pub struct HistoryLog {
    pages: VecDeque<Arc<Page>>,
    len: usize,
}

impl HistoryLog {
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
        edit(&mut page.rows[row]);
        page.payload_bytes += group_bytes(&page.rows[row]);
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
        + group
            .lines
            .iter()
            .map(|line| {
                line.speaker_id.as_ref().map_or(0, String::capacity)
                    + line.speaker_name.capacity()
                    + line.content.id.capacity()
                    + line.content.fallback.storage_bytes()
            })
            .sum::<usize>()
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
    use crate::{Epoch, HistoryGroupId, InteractionId, SessionId, SessionIdentity};

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
}

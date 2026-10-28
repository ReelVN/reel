//! Single-owner PCM blocks move between the worker and reader, never sharing mutable samples.
use super::*;
use rtrb::{Consumer, Producer, RingBuffer};

const BLOCK_FRAMES: usize = 128;
pub(super) struct Block {
    pub(super) start: usize,
    pub(super) len: usize,
    pub(super) serial: u64,
    pub(super) frames: Box<[Frame]>,
}
impl Block {
    fn contains(&self, index: usize) -> bool {
        index >= self.start && index - self.start < self.len
    }
}
pub(super) struct Reader {
    ready: Consumer<Block>,
    recycled: Producer<Block>,
    current: Option<Block>,
    history: std::collections::VecDeque<Block>,
    pub(super) serial: u64,
    next: Option<usize>,
    blocks: usize,
}
pub(super) struct Writer {
    ready: Producer<Block>,
    recycled: Consumer<Block>,
}
pub(super) fn storage_bytes(frames: usize) -> Option<u64> {
    let count = frames.div_ceil(BLOCK_FRAMES);
    frames
        .checked_mul(size_of::<Frame>())?
        .checked_add(count.checked_mul(3 * size_of::<Block>())?)?
        .checked_add(2 * (size_of::<RingBuffer<Block>>() + 2 * size_of::<usize>()))?
        .checked_add(size_of::<Writer>())
        .filter(|bytes| *bytes <= isize::MAX as usize)
        .map(|bytes| bytes as u64)
}
pub(super) fn channels(frames: usize) -> (Reader, Writer) {
    let count = frames.div_ceil(BLOCK_FRAMES);
    let (ready_out, ready_in) = RingBuffer::new(count);
    let (mut free_out, free_in) = RingBuffer::new(count);
    for start in (0..frames).step_by(BLOCK_FRAMES) {
        free_out
            .push(Block {
                start: 0,
                len: 0,
                serial: 0,
                frames: vec![Frame::ZERO; (frames - start).min(BLOCK_FRAMES)].into_boxed_slice(),
            })
            .unwrap_or_else(|_| unreachable!("one slot per owned PCM block"));
    }
    (
        Reader {
            ready: ready_in,
            recycled: free_out,
            current: None,
            history: std::collections::VecDeque::with_capacity(count),
            serial: 0,
            next: None,
            blocks: count,
        },
        Writer {
            ready: ready_out,
            recycled: free_in,
        },
    )
}
impl Reader {
    fn recycle_current(&mut self) -> bool {
        if self.current.is_none() {
            return true;
        }
        // Reserve before extracting the Box. Even a full queue preserves its owner.
        let Ok(chunk) = self.recycled.write_chunk_uninit(1) else {
            return false;
        };
        chunk.fill_from_iter(std::iter::once(
            self.current.take().expect("owned PCM block"),
        ));
        true
    }
    fn recycle_history(&mut self) -> bool {
        if self.history.is_empty() {
            return true;
        }
        let Ok(chunk) = self.recycled.write_chunk_uninit(1) else {
            return false;
        };
        chunk.fill_from_iter(std::iter::once(
            self.history.pop_front().expect("owned PCM history"),
        ));
        true
    }
    fn clear(&mut self) -> bool {
        if !self.recycle_current() {
            return false;
        }
        while !self.history.is_empty() {
            if !self.recycle_history() {
                return false;
            }
        }
        true
    }
    fn retain_current(&mut self) -> bool {
        if self.current.is_some() {
            // One fixed slot per pool block. Retaining never grows the allocation.
            if self.history.len() == self.history.capacity() {
                return false;
            }
            self.history
                .push_back(self.current.take().expect("owned PCM block"));
        }
        true
    }
    pub(super) fn seek_cached(&mut self, window: &Window, index: usize) -> bool {
        let mut frame = [Frame::ZERO];
        self.read_inner(window, index, &mut frame, false) != 0
    }
    pub(super) fn read(&mut self, window: &Window, index: usize, output: &mut [Frame]) -> usize {
        self.read_inner(window, index, output, true)
    }
    fn read_inner(
        &mut self,
        window: &Window,
        index: usize,
        output: &mut [Frame],
        prepare_missing: bool,
    ) -> usize {
        let serial = window.serial.load(Ordering::Acquire);
        if self.serial != serial {
            if !self.clear() {
                return 0;
            }
            self.serial = serial;
            self.next = None;
        }
        let mut retained = self
            .current
            .as_ref()
            .is_some_and(|block| block.contains(index));
        if !retained
            && (self.next != Some(index)
                || !self.ready.peek().is_ok_and(|block| block.contains(index)))
            && let Some(found) = self.history.iter().position(|block| block.contains(index))
        {
            if !self.retain_current() {
                return 0;
            }
            self.current = self.history.remove(found);
            retained = true;
        }
        if prepare_missing && !retained && self.next.is_some_and(|next| next != index) {
            window.wanted.store(index, Ordering::Release);
            self.serial = window.serial.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
            if !self.clear() {
                return 0;
            }
            self.next = None;
        }
        window.wanted.store(index, Ordering::Release);
        for _ in 0..=self.blocks {
            if self.current.is_none() {
                self.current = self.ready.pop().ok();
            }
            let Some(block) = &self.current else {
                // The worker may be waiting for a block while this caller needs
                // uncached data. Return one old owner without waiting for it.
                self.recycle_history();
                return 0;
            };
            if block.serial != self.serial {
                if !self.recycle_current() {
                    return 0;
                }
                continue;
            }
            if index >= block.start.saturating_add(block.len) {
                if !self.retain_current() {
                    return 0;
                }
                continue;
            }
            if !block.contains(index) {
                return 0;
            }
            let offset = index - block.start;
            let count = output.len().min(block.len - offset);
            output[..count].copy_from_slice(&block.frames[offset..offset + count]);
            let exhausted = offset + count == block.len;
            if window.serial.load(Ordering::Acquire) != self.serial {
                return 0;
            }
            self.next = Some(index + count);
            // The caller owns this copied prefix, so the worker can prepare past it.
            window.wanted.store(index + count, Ordering::Release);
            if exhausted && !self.retain_current() {
                return 0;
            }
            if window.needs_block.load(Ordering::Acquire) {
                self.recycle_history();
            }
            return count;
        }
        0
    }
}
impl Writer {
    pub(super) fn take(&mut self) -> Option<Block> {
        self.recycled.pop().ok().map(|mut block| {
            block.len = 0;
            block
        })
    }
    pub(super) fn publish(&mut self, block: Block) {
        self.ready
            .push(block)
            .unwrap_or_else(|_| unreachable!("owned PCM block publication credit"));
    }
}

#[cfg(test)]
mod tests;

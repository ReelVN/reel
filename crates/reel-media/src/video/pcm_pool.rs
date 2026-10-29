//! One worker-owned PCM block; the publication ring owns independent samples.
use reel_core::budget::{BudgetPool, BudgetTicket};

const FRAMES: usize = 1024;
const BYTES: u64 = (FRAMES * std::mem::size_of::<[f32; 2]>()) as u64;

#[derive(Default)]
pub(super) struct PcmPool {
    frames: Vec<[f32; 2]>,
    storage: Option<BudgetTicket>,
}
impl PcmPool {
    pub fn prepare(&mut self, budget: &BudgetPool) -> Result<&mut Vec<[f32; 2]>, String> {
        if self.storage.is_none() {
            let mut ticket = budget
                .reserve(BYTES)
                .map_err(|e| super::budget_failure(budget, "PCM pool", BYTES, e).message)?;
            self.frames
                .try_reserve_exact(FRAMES)
                .map_err(|e| e.to_string())?;
            ticket
                .resize((self.frames.capacity() * 8) as u64)
                .map_err(|e| {
                    super::budget_failure(
                        budget,
                        "PCM pool resize",
                        (self.frames.capacity() * 8) as u64,
                        e,
                    )
                    .message
                })?;
            self.storage = Some(ticket);
        }
        self.frames.clear();
        Ok(&mut self.frames)
    }
    pub fn publish(&mut self) -> (Vec<[f32; 2]>, BudgetTicket) {
        // The vector and its existing reservation move together to the worker.
        (
            std::mem::take(&mut self.frames),
            self.storage.take().expect("prepared PCM"),
        )
    }
    pub fn recycle(&mut self, mut frames: Vec<[f32; 2]>, storage: BudgetTicket) {
        frames.clear();
        self.frames = frames;
        self.storage = Some(storage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::budget::{BudgetCategory, QueueLimits};

    #[test]
    fn repeated_publication_reuses_one_block_and_keeps_idle_capacity_charged() {
        let budget = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 4,
                max_bytes: BYTES,
            },
        );
        let mut pool = PcmPool::default();
        let mut pointer = None;
        for _ in 0..10_000 {
            let frames = pool.prepare(&budget).unwrap();
            assert_eq!(*pointer.get_or_insert(frames.as_ptr()), frames.as_ptr());
            frames.extend_from_slice(&[[0.25, -0.25]; FRAMES]);
            let (frames, output) = pool.publish();
            assert_eq!(budget.usage().bytes, BYTES);
            assert_eq!(frames[FRAMES - 1], [0.25, -0.25]);
            pool.recycle(frames, output);
            assert_eq!(budget.usage().bytes, BYTES);
        }
        assert_eq!(budget.usage().peak_bytes, BYTES);
        drop(pool);
        assert_eq!(budget.usage().bytes, 0);
    }

    #[test]
    fn pending_poll_and_seek_trim_keep_the_same_block_until_retirement() {
        let budget = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 2,
                max_bytes: BYTES,
            },
        );
        let mut pool = PcmPool::default();
        let pointer = pool.prepare(&budget).unwrap().as_ptr();
        for _ in 0..100 {
            let frames = pool.prepare(&budget).unwrap();
            frames.extend_from_slice(&[[1., 1.]; 960]);
            frames.drain(..480);
            assert_eq!(frames.as_ptr(), pointer);
        }
        assert_eq!(budget.usage().bytes, BYTES);
        drop(pool);
        assert_eq!(budget.usage().bytes, 0);
    }
}

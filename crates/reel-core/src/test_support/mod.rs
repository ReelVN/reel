//! Executable contract fixtures, enabled explicitly with `test-support`.
//! No filesystem, device, VM, production codecs or durability simulation.
mod cooperative;
mod memory_io;
mod queue;

pub use cooperative::COMPLETION_BYTES as WORK_COMPLETION_BYTES;
pub use cooperative::CooperativeExecutor;
pub use memory_io::COMPLETION_BYTES as IO_COMPLETION_BYTES;
pub use memory_io::{MemoryAsset, MemoryAssetIo, StoredSave};
pub use queue::QueueUsage;

use crate::clock::{Clock, MonoTime, UnixMillis};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Debug, Default)]
pub struct FakeClock {
    mono: Rc<Cell<MonoTime>>,
    wall: Rc<Cell<Option<UnixMillis>>>,
    step: Rc<Cell<u64>>,
}

impl FakeClock {
    pub fn set_monotonic(&self, now: MonoTime) {
        assert!(
            now >= self.mono.get(),
            "monotonic clock cannot go backwards"
        );
        self.mono.set(now);
    }
    pub fn set_wall_time(&self, now: Option<UnixMillis>) {
        self.wall.set(now);
    }
    /// Test deadline checks without sleeping; each observation advances by this amount.
    pub fn set_observation_step(&self, nanos: u64) {
        self.step.set(nanos);
    }
}

impl Clock for FakeClock {
    fn monotonic_now(&self) -> MonoTime {
        let now = self.mono.get();
        self.mono.set(MonoTime(
            now.0
                .checked_add(self.step.get())
                .expect("fake clock overflow"),
        ));
        now
    }
    fn wall_time(&self) -> Option<UnixMillis> {
        self.wall.get()
    }
}

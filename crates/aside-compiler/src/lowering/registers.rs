use super::{Error, ErrorKind, budget::Budget};
use aside_vm::bytecode::{Reg, RegWindow};

/// Pinned lexical slots precede a sorted, coalescing free interval list.
/// No slot occupied by a live expression can enter a call window.
#[derive(Debug)]
pub(crate) struct Registers {
    pub high: u32,
    pinned: u32,
    free: Vec<RegWindow>,
}
impl Registers {
    pub fn new(pinned: u32, b: &Budget) -> Result<Self, Box<Error>> {
        if pinned > b.limits.max_registers {
            return Err(b.error(ErrorKind::Limit, "fixed register slots exceed limit"));
        }
        Ok(Self {
            high: pinned,
            pinned,
            free: Vec::new(),
        })
    }
    pub fn get(&mut self, len: u32, b: &mut Budget) -> Result<RegWindow, Box<Error>> {
        b.work(self.free.len() as u64 + 1)?;
        if len == 0 {
            return Ok(RegWindow {
                start: Reg(self.pinned),
                len: 0,
            });
        }
        if let Some(i) = self.free.iter().position(|w| w.len >= len) {
            let start = self.free[i].start;
            self.free[i].start.0 += len;
            self.free[i].len -= len;
            if self.free[i].len == 0 {
                self.free.remove(i);
            }
            return Ok(RegWindow { start, len });
        }
        let end = self
            .high
            .checked_add(len)
            .filter(|&n| n <= b.limits.max_registers)
            .ok_or_else(|| b.error(ErrorKind::Limit, "temporary register window exceeds limit"))?;
        let start = Reg(self.high);
        self.high = end;
        Ok(RegWindow { start, len })
    }
    pub fn temp(&mut self, b: &mut Budget) -> Result<Reg, Box<Error>> {
        Ok(self.get(1, b)?.start)
    }
    pub fn release(&mut self, window: RegWindow, b: &mut Budget) -> Result<(), Box<Error>> {
        if window.len == 0 {
            return Ok(());
        }
        b.work(self.free.len() as u64 + 1)?;
        let end = window
            .start
            .0
            .checked_add(window.len)
            .filter(|&end| window.start.0 >= self.pinned && end <= self.high)
            .ok_or_else(|| b.error(ErrorKind::InvalidIr, "release of a fixed/outside register"))?;
        let i = self.free.partition_point(|w| w.start < window.start);
        if i > 0 && self.free[i - 1].start.0 + self.free[i - 1].len > window.start.0
            || i < self.free.len() && end > self.free[i].start.0
        {
            return Err(b.error(ErrorKind::InvalidIr, "overlapping register release"));
        }
        b.reserve(&mut self.free, 1)?;
        self.free.insert(i, window);
        if i + 1 < self.free.len() && end == self.free[i + 1].start.0 {
            let next = self.free.remove(i + 1);
            self.free[i].len += next.len;
        }
        if i > 0 && self.free[i - 1].start.0 + self.free[i - 1].len == self.free[i].start.0 {
            let current = self.free.remove(i);
            self.free[i - 1].len += current.len;
        }
        Ok(())
    }
    pub fn drop_temp(&mut self, reg: Reg, b: &mut Budget) -> Result<(), Box<Error>> {
        self.release(RegWindow { start: reg, len: 1 }, b)
    }
    pub fn finish(self, b: &mut Budget) {
        b.release(self.free);
    }
}

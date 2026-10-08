use super::{Error, ErrorKind, Limits, Stats};
use crate::{TextRange, TextSize};
use std::mem::size_of;

#[derive(Debug)]
pub(crate) struct Budget {
    pub limits: Limits,
    pub stats: Stats,
    pub span: TextRange,
}
impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            stats: Stats::default(),
            span: TextRange::empty(TextSize::from(0)),
        }
    }
    pub fn error(&self, kind: ErrorKind, detail: impl Into<String>) -> Error {
        Error {
            kind,
            span: self.span,
            detail: detail.into(),
            stats: self.stats,
        }
    }
    pub fn work(&mut self, n: u64) -> Result<(), Error> {
        let next = self
            .stats
            .work_units
            .checked_add(n)
            .filter(|&n| n <= self.limits.max_work)
            .ok_or_else(|| self.error(ErrorKind::Limit, "lowering work budget exceeded"))?;
        self.stats.work_units = next;
        Ok(())
    }
    pub fn record(&mut self) -> Result<(), Error> {
        if self.stats.records >= self.limits.max_records.min(u32::MAX as usize) {
            return Err(self.error(ErrorKind::Limit, "lowering record budget exceeded"));
        }
        self.stats.records += 1;
        self.work(1)
    }
    pub fn capacity(&mut self, bytes: usize) -> Result<(), Error> {
        let next = self
            .stats
            .capacity_bytes
            .checked_add(bytes)
            .filter(|&n| n <= self.limits.max_capacity_bytes)
            .ok_or_else(|| self.error(ErrorKind::Limit, "lowering capacity budget exceeded"))?;
        self.stats.capacity_bytes = next;
        self.stats.capacity_peak_bytes = self.stats.capacity_peak_bytes.max(next);
        Ok(())
    }
    pub fn release_bytes(&mut self, bytes: usize) {
        self.stats.capacity_bytes -= bytes;
    }
    /// Charge the complete replacement allocation while the old allocation still
    /// exists. try_reserve_exact may realloc in place; the peak is conservative.
    pub fn reserve<T>(&mut self, v: &mut Vec<T>, additional: usize) -> Result<(), Error> {
        let needed = v
            .len()
            .checked_add(additional)
            .ok_or_else(|| self.error(ErrorKind::Limit, "vector length overflow"))?;
        if needed <= v.capacity() {
            return Ok(());
        }
        let desired = needed.checked_next_power_of_two().unwrap_or(needed);
        let new_bytes = desired
            .checked_mul(size_of::<T>())
            .ok_or_else(|| self.error(ErrorKind::Limit, "vector capacity overflow"))?;
        self.work(desired as u64)?;
        self.capacity(new_bytes)?;
        let old = v.capacity() * size_of::<T>();
        v.try_reserve_exact(desired - v.len())
            .map_err(|_| self.error(ErrorKind::Allocation, "lowering vector reservation failed"))?;
        let actual = v.capacity() * size_of::<T>();
        if actual > new_bytes {
            self.capacity(actual - new_bytes)?;
        } else {
            self.release_bytes(new_bytes - actual);
        }
        self.release_bytes(old);
        Ok(())
    }
    pub fn push<T>(&mut self, v: &mut Vec<T>, value: T) -> Result<(), Error> {
        self.record()?;
        self.reserve(v, 1)?;
        v.push(value);
        Ok(())
    }
    pub fn filled<T: Clone>(&mut self, len: usize, value: T) -> Result<Vec<T>, Error> {
        self.work(len as u64)?;
        let mut v = Vec::new();
        self.reserve(&mut v, len)?;
        v.resize(len, value);
        Ok(v)
    }
    pub fn release<T>(&mut self, v: Vec<T>) {
        self.release_bytes(v.capacity() * size_of::<T>());
    }
    pub fn string(&mut self, text: &str) -> Result<String, Error> {
        self.work(text.len() as u64)?;
        let mut v = Vec::new();
        self.reserve(&mut v, text.len())?;
        v.extend_from_slice(text.as_bytes());
        Ok(String::from_utf8(v).expect("source is UTF-8"))
    }
    pub fn compare(&mut self, a: &str, b: &str) -> Result<std::cmp::Ordering, Error> {
        self.work(1 + a.len().min(b.len()) as u64)?;
        Ok(a.cmp(b))
    }
}

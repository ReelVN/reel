//! Budgeted Zeno stroke output, shared by bounds and rasterization.
use super::{GpuError, zeno};
use reel_core::budget::{BudgetPool, BudgetTicket};
use zeno::{Command, PathBuilder, Point};

pub(super) struct StrokePath {
    commands: Vec<Command>,
    storage: BudgetTicket,
    available: u64,
    error: Option<GpuError>,
}
impl StrokePath {
    pub(super) fn new(budget: &BudgetPool, available: u64) -> Result<Self, GpuError> {
        Ok(Self {
            commands: Vec::new(),
            storage: budget.reserve(0).map_err(|_| GpuError::Budget {
                domain: "glyph stroke path",
                requested: 0,
                available,
            })?,
            available,
            error: None,
        })
    }
    pub(super) fn finish(&mut self) -> Result<(), GpuError> {
        self.error.take().map_or(Ok(()), Err)
    }
    pub(super) fn commands(&self) -> &[Command] {
        &self.commands
    }
    pub(super) fn bytes(&self) -> u64 {
        self.storage.bytes()
    }
    fn push(&mut self, command: Command) -> &mut Self {
        if self.error.is_some() {
            return self;
        }
        if self.commands.len() == self.commands.capacity() {
            let old_bytes = self.commands.capacity() as u64 * size_of::<Command>() as u64;
            let capacity = self.commands.capacity().saturating_mul(2).max(64).min(
                (self.available.saturating_sub(old_bytes) / size_of::<Command>() as u64) as usize,
            );
            let bytes = capacity.max(self.commands.len() + 1) as u64 * size_of::<Command>() as u64;
            let peak = old_bytes + bytes;
            if peak > self.available || self.storage.resize(peak).is_err() {
                self.error = Some(GpuError::Budget {
                    domain: "glyph stroke path",
                    requested: peak,
                    available: self.available,
                });
                return self;
            }
            self.commands.reserve_exact(capacity - self.commands.len());
            self.storage
                .resize(self.commands.capacity() as u64 * size_of::<Command>() as u64)
                .expect("release stroke path growth overlap");
        }
        self.commands.push(command);
        self
    }
}
impl PathBuilder for StrokePath {
    fn current_point(&self) -> Point {
        self.commands.current_point()
    }
    fn move_to(&mut self, to: impl Into<Point>) -> &mut Self {
        self.push(Command::MoveTo(to.into()))
    }
    fn line_to(&mut self, to: impl Into<Point>) -> &mut Self {
        self.push(Command::LineTo(to.into()))
    }
    fn curve_to(
        &mut self,
        control1: impl Into<Point>,
        control2: impl Into<Point>,
        to: impl Into<Point>,
    ) -> &mut Self {
        self.push(Command::CurveTo(
            control1.into(),
            control2.into(),
            to.into(),
        ))
    }
    fn quad_to(&mut self, control: impl Into<Point>, to: impl Into<Point>) -> &mut Self {
        self.push(Command::QuadTo(control.into(), to.into()))
    }
    fn close(&mut self) -> &mut Self {
        self.push(Command::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stroke_path_stops_allocating_at_its_budget_and_releases_storage() {
        let budget = crate::common::renderer::Budgets::new(Default::default()).transient;
        let limit = 2 * size_of::<Command>() as u64;
        let mut path = StrokePath::new(&budget, limit).unwrap();
        path.move_to((0., 0.)).line_to((4., 0.));
        assert_eq!(budget.usage().bytes, limit);
        path.line_to((4., 4.)).close();
        assert_eq!(path.commands().len(), 2);
        assert!(matches!(
            path.finish(),
            Err(GpuError::Budget {
                domain: "glyph stroke path",
                ..
            })
        ));
        assert_eq!(budget.usage().bytes, limit);
        drop(path);
        assert_eq!(budget.usage().bytes, 0);
    }
}

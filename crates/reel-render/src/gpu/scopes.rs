use super::GpuError;
use reel_stage::RecordLocation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Clip,
    Group,
}
pub(super) struct Entry<T> {
    pub(super) at: RecordLocation,
    pub(super) value: T,
    kind: Kind,
}
/// The replay stack owns its payloads. Validation precedes allocation/removal.
pub(super) struct Scopes<T> {
    root: T,
    nested: Vec<Entry<T>>,
    base: usize,
    limit: usize,
}
impl<T> Scopes<T> {
    pub(super) fn new(
        base: usize,
        limit: usize,
        at: RecordLocation,
        make: impl FnOnce() -> Result<T, GpuError>,
    ) -> Result<Self, GpuError> {
        if base >= limit {
            return Err(GpuError::DepthLimit { at, limit });
        }
        Ok(Self {
            root: make()?,
            nested: Vec::new(),
            base,
            limit,
        })
    }
    pub(super) fn depth(&self) -> usize {
        self.base + 1 + self.nested.len()
    }
    pub(super) fn push(
        &mut self,
        kind: Kind,
        at: RecordLocation,
        make: impl FnOnce() -> Result<T, GpuError>,
    ) -> Result<(), GpuError> {
        if self.depth() >= self.limit {
            return Err(GpuError::DepthLimit {
                at,
                limit: self.limit,
            });
        }
        self.nested.push(Entry {
            at,
            value: make()?,
            kind,
        });
        Ok(())
    }
    pub(super) fn pop(&mut self, kind: Kind, at: RecordLocation) -> Result<Entry<T>, GpuError> {
        match self.nested.last() {
            None => {
                return Err(GpuError::InvalidRecord {
                    at,
                    record: "scope underflow",
                });
            }
            Some(entry) if entry.kind != kind => {
                return Err(GpuError::InvalidRecord {
                    at,
                    record: "mismatched scope terminator",
                });
            }
            _ => (),
        }
        Ok(self.nested.pop().expect("validated scope"))
    }
    pub(super) fn last(&self) -> &T {
        self.nested.last().map_or(&self.root, |entry| &entry.value)
    }
    pub(super) fn finish(self) -> Result<T, GpuError> {
        if let Some(entry) = self.nested.last() {
            return Err(GpuError::InvalidRecord {
                at: entry.at,
                record: "unclosed scope",
            });
        }
        Ok(self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use masonry_core::core::Widget;
    fn at(command: usize) -> RecordLocation {
        // One stable owner is needed to compare locations across calls.
        static OWNER: std::sync::OnceLock<masonry_core::core::WidgetId> =
            std::sync::OnceLock::new();
        RecordLocation {
            fragment: 0,
            command,
            mask_depth: 0,
            owner: *OWNER.get_or_init(|| masonry::widgets::Label::new("").prepare().id()),
        }
    }
    #[test]
    fn boundary_rejects_before_allocation_and_checks_recursive_root() {
        let mut stack = Scopes::new(0, 2, at(0), || Ok(())).unwrap();
        stack.push(Kind::Group, at(1), || Ok(())).unwrap();
        let error = stack
            .push(Kind::Clip, at(2), || panic!("must not allocate"))
            .unwrap_err();
        assert!(
            matches!(error, GpuError::DepthLimit { at: location, limit: 2 } if location == at(2))
        );
        assert!(!error.is_retryable());
        stack.pop(Kind::Group, at(3)).unwrap();
        assert_eq!(stack.depth(), 1);
        assert!(Scopes::new(stack.depth(), 2, at(4), || Ok(())).is_ok());
        assert!(
            matches!(Scopes::<()>::new(2, 2, at(5), || panic!("must not allocate")), Err(GpuError::DepthLimit { at: location, .. }) if location == at(5))
        );
        stack.finish().unwrap();
    }
    #[test]
    fn malformed_structure_is_localized_and_permanent() {
        let mut stack = Scopes::new(0, 3, at(0), || Ok(())).unwrap();
        assert!(
            matches!(stack.pop(Kind::Clip, at(1)), Err(GpuError::InvalidRecord { at: location, record: "scope underflow" }) if location == at(1))
        );
        stack.push(Kind::Group, at(2), || Ok(())).unwrap();
        assert!(
            matches!(stack.pop(Kind::Clip, at(3)), Err(GpuError::InvalidRecord { at: location, record: "mismatched scope terminator" }) if location == at(3))
        );
        let error = stack.finish().unwrap_err();
        assert!(
            matches!(error, GpuError::InvalidRecord { at: location, record: "unclosed scope" } if location == at(2))
        );
        assert!(!error.is_retryable());
    }
}

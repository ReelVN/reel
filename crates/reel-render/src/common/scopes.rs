use super::GpuError;
use reel_ui::frame::RecordLocation;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Clip,
    Group,
}
pub(crate) struct Entry<T> {
    pub(crate) at: RecordLocation,
    pub(crate) value: T,
    kind: Kind,
}
/// The replay stack owns its payloads. Validation precedes allocation/removal.
pub(crate) struct Scopes<T> {
    root: T,
    nested: Vec<Entry<T>>,
}

pub(crate) fn push_stack<T>(
    stack: &mut Vec<T>,
    entry: T,
    reserve: impl FnOnce(u64) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    if stack.len() == stack.capacity() {
        let additional = stack.capacity().max(1);
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(GpuError::InvalidSize)?;
        reserve(bytes as u64)?;
        stack.reserve_exact(additional);
    }
    stack.push(entry);
    Ok(())
}

impl<T> Scopes<T> {
    pub(crate) fn new(root: T) -> Self {
        Self {
            root,
            nested: Vec::new(),
        }
    }
    pub(crate) fn push(
        &mut self,
        kind: Kind,
        at: RecordLocation,
        value: T,
        reserve: impl FnOnce(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        push_stack(&mut self.nested, Entry { at, value, kind }, reserve)
    }
    pub(crate) fn pop(&mut self, kind: Kind, at: RecordLocation) -> Result<Entry<T>, GpuError> {
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
    pub(crate) fn last(&self) -> &T {
        self.nested.last().map_or(&self.root, |entry| &entry.value)
    }
    pub(crate) fn finish(self) -> Result<T, GpuError> {
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
    use reel_ui::core::Widget;
    fn at(command: usize) -> RecordLocation {
        // One stable owner is needed to compare locations across calls.
        static OWNER: std::sync::OnceLock<reel_ui::core::WidgetId> = std::sync::OnceLock::new();
        RecordLocation {
            fragment: 0,
            command,
            mask_depth: 0,
            owner: *OWNER.get_or_init(|| reel_ui::widgets::Label::new("").prepare().id()),
        }
    }
    #[test]
    fn stack_growth_reserves_capacity_before_mutation_without_a_depth_ceiling() {
        let mut stack = Scopes::new(());
        let mut charged = 0;
        for index in 0..128 {
            stack
                .push(Kind::Group, at(index), (), |bytes| {
                    charged += bytes;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(
            charged,
            (stack.nested.capacity() * size_of::<Entry<()>>()) as u64
        );
        assert!(matches!(
            stack.push(Kind::Clip, at(128), (), |bytes| {
                Err(GpuError::Budget {
                    domain: "transient",
                    requested: bytes,
                    available: 0,
                })
            }),
            Err(GpuError::Budget { .. })
        ));
        assert_eq!(stack.nested.len(), 128);
        for index in (0..128).rev() {
            assert_eq!(stack.pop(Kind::Group, at(index)).unwrap().at, at(index));
        }
        stack.finish().unwrap();
    }
    #[test]
    fn malformed_structure_is_localized_and_permanent() {
        let mut stack = Scopes::new(());
        assert!(
            matches!(stack.pop(Kind::Clip, at(1)), Err(GpuError::InvalidRecord { at: location, record: "scope underflow" }) if location == at(1))
        );
        stack.push(Kind::Group, at(2), (), |_| Ok(())).unwrap();
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

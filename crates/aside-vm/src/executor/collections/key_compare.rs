//! Complete short text-key comparisons inside their admitted scan step.
use super::*;

const KEY_BYTES: usize = 32;

impl Vm {
    pub(super) fn compare_table_text_key(
        &mut self,
        frame: &mut ScriptFrame,
        left: Value,
        right: Value,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(test)]
        if self.work_probe.general_table_key_comparison {
            return Ok(false);
        }
        if left.tag() != ValueTag::Object || right.tag() != ValueTag::Object {
            return Ok(false);
        }
        let a = self.heap.info(left)?;
        let b = self.heap.info(right)?;
        if !matches!(a.kind, ObjectKind::String | ObjectKind::Bytes) {
            return Ok(false);
        }
        let mismatched_shape = a.kind != b.kind || a.len != b.len;
        if !mismatched_shape && a.len as usize > KEY_BYTES {
            return Ok(false);
        }
        // A mismatch needs comparison, root cleanup and scan continuation.
        // Equal text needs each byte, the end check and scan continuation.
        // Keep the original resumable path when the complete sequence will not
        // fit; no new minimum-work or soft-clock boundary is introduced.
        let maximum = if mismatched_shape {
            3
        } else {
            a.len.max(1) + 2
        };
        if !meter.admits_short_steps(maximum as u8)
            || meter.budget.work - meter.usage.work < u64::from(maximum)
        {
            return Ok(false);
        }
        let (equal, steps) = if mismatched_shape {
            (false, 3)
        } else {
            let len = a.len as usize;
            let mut a = [0; KEY_BYTES];
            let mut b = [0; KEY_BYTES];
            self.heap.read_bytes(left, 0, &mut a[..len])?;
            self.heap.read_bytes(right, 0, &mut b[..len])?;
            match a[..len].iter().zip(&b[..len]).position(|(a, b)| a != b) {
                Some(index) => (false, index + 3),
                None => (true, len + 2),
            }
        };
        for _ in 0..steps {
            assert!(meter.next_short_step());
            assert!(meter.take(0, 1, 0));
        }
        if frame.table_work.unwrap().named_read(frame.member_symbol) {
            execution_count!(self, NamedTableEqualitySteps, steps as u64);
        }
        // Both keys remain owned by the query/table throughout this synchronous
        // comparison. The general equality walk's temporary roots are unnecessary.
        self.table_comparison_done(frame, Ok(equal))?;
        Ok(true)
    }
}

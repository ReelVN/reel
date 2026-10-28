use super::*;

impl SessionVm {
    pub(super) fn virtual_anchor(&mut self, s: Value) -> Result<(), AdapterError> {
        let scroll = get(self.vm.heap(), s, S::Scroll)?;
        if scroll.tag() == ValueTag::Null {
            return Ok(());
        }
        let property = aside_vm::PropertyId {
            host: scroll,
            field: stage::scroll::ScrollField::Y.index() as u32,
        };
        let previous = num(self.vm.heap(), s, S::Offset)?;
        if stage::number(self.vm.heap().host_property(property)?)? != previous {
            return Ok(());
        }
        let n = count(self.vm.heap(), s)?;
        let old_index = int(self.vm.heap(), s, S::AnchorIndex)?;
        let start = num(self.vm.heap(), s, S::Start)?;
        let end = num(self.vm.heap(), s, S::End)?;
        let next = if get(self.vm.heap(), s, S::AtEnd)?.as_bool() == Some(true) {
            let fields = self.vm.heap().host_fields(node(self.vm.heap(), s)?)?;
            let padding = stage::number(
                self.vm
                    .heap()
                    .table_entry(fields, stage::NodeProperty::Padding.index() as u32)?
                    .1,
            )?;
            let offset = (total(self.vm.heap(), s)? + padding * 2. - (end - start)).max(0.);
            start + offset - previous
        } else if n > 0 && old_index >= 0 {
            let key = get(self.vm.heap(), s, S::AnchorKey)?;
            let found = find(
                self.vm.heap(),
                get(self.vm.heap(), s, S::Sorted)?,
                key,
                self.read_limit,
            )?;
            let i = if let Some(e) = found {
                self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32
            } else {
                (old_index as u32).min(n - 1)
            };
            prefix(self.vm.heap(), s, i)? + num(self.vm.heap(), s, S::AnchorOffset)?
        } else {
            0.
        };
        let delta = next - start;
        let offset = (previous + delta).max(0.);
        self.vm
            .heap_mut()
            .write_host_property(property, Value::float(offset), true)?;
        put(self.vm.heap_mut(), s, S::Offset, Value::float(offset))?;
        put(self.vm.heap_mut(), s, S::Start, Value::float(next))?;
        put(self.vm.heap_mut(), s, S::End, Value::float(end + delta))?;
        Ok(())
    }

    pub(in crate::aside) fn virtual_feedback(
        &mut self,
        event: &NodeVirtualList,
    ) -> Result<bool, AdapterError> {
        let Some(scene) = self.stage.scene.node(event.node) else {
            return Ok(false);
        };
        let NodeKind::VirtualList(view) = &scene.kind else {
            return Ok(false);
        };
        let view = view.clone();
        if view.revision != event.revision {
            return Ok(false);
        }
        let host = self.stage.value(self.vm.heap(), event.node)?;
        let s = state(self.vm.heap(), host)?;
        let scroll = self.stage.value(self.vm.heap(), event.scroll)?;
        let scroll_y = aside_vm::PropertyId {
            host: scroll,
            field: stage::scroll::ScrollField::Y.index() as u32,
        };
        if stage::number(self.vm.heap().host_property(scroll_y)?)? != event.offset.y {
            return Ok(false);
        }
        let follow = matches!(
            self.stage.scene.node(event.scroll).map(|n| &n.kind),
            Some(NodeKind::Scroll {
                follow_end: true,
                ..
            })
        );
        let mut changed = self.virtual_target_feedback(s, event)?;
        if get(self.vm.heap(), s, S::Target)?.tag() != ValueTag::Null
            && num(self.vm.heap(), s, S::End)? - num(self.vm.heap(), s, S::Start)?
                != event.visible_end - event.visible_start
        {
            changed = true;
        }
        let moved = num(self.vm.heap(), s, S::Start)? != event.visible_start
            || num(self.vm.heap(), s, S::End)? != event.visible_end;
        for (field, value) in [
            (S::Start, event.visible_start),
            (S::End, event.visible_end),
            (S::Offset, event.offset.y),
        ] {
            if num(self.vm.heap(), s, field)? != value {
                put(self.vm.heap_mut(), s, field, Value::float(value))?;
            }
        }
        put(self.vm.heap_mut(), s, S::Scroll, scroll)?;
        let at_end = follow && event.visible_end >= total(self.vm.heap(), s)? - 0.01;
        put(self.vm.heap_mut(), s, S::AtEnd, Value::bool(at_end))?;
        if count(self.vm.heap(), s)? > 0 {
            let i = locate(self.vm.heap(), s, event.visible_start)?;
            let e = self
                .vm
                .heap()
                .sequence_get(get(self.vm.heap(), s, S::Order)?, i)?;
            let key = entry(self.vm.heap(), e, Entry::Key)?;
            let offset = event.visible_start - prefix(self.vm.heap(), s, i)?;
            put(self.vm.heap_mut(), s, S::AnchorKey, key)?;
            put(
                self.vm.heap_mut(),
                s,
                S::AnchorIndex,
                Value::int(i64::from(i)),
            )?;
            put(self.vm.heap_mut(), s, S::AnchorOffset, Value::float(offset))?;
        }
        if num(self.vm.heap(), s, S::Width)? != event.width {
            let epoch = int(self.vm.heap(), s, S::Epoch)? + 1;
            put(self.vm.heap_mut(), s, S::Epoch, Value::int(epoch))?;
            put(self.vm.heap_mut(), s, S::Width, Value::float(event.width))?;
            changed = true;
        }
        let mounted = get(self.vm.heap(), s, S::Mounted)?;
        let _measurement_storage = self
            .vm
            .heap()
            .budget_usage()
            .reserve((event.measurements.len() * size_of::<usize>()) as u64)?;
        let mut measurement_order = Vec::new();
        measurement_order
            .try_reserve_exact(event.measurements.len())
            .map_err(|_| AdapterError::Allocation)?;
        measurement_order.extend(0..event.measurements.len());
        measurement_order.sort_unstable_by_key(|&i| (event.measurements[i].node, i));
        let mut retained = Vec::new();
        for i in 0..self.vm.heap().sequence_len(mounted)? {
            let e = self.vm.heap().sequence_get(mounted, i)?;
            let child = entry(self.vm.heap(), e, Entry::Node)?;
            let id = NodeId(self.vm.heap().host_payload(child, 0)?.as_int().unwrap() as u64);
            if event.retained.contains(&id) {
                retained.push(entry(self.vm.heap(), e, Entry::Key)?);
            }
            let found = measurement_order.partition_point(|&i| event.measurements[i].node < id);
            if let Some(measurement) = measurement_order
                .get(found)
                .map(|&i| &event.measurements[i])
                .filter(|m| m.node == id)
            {
                if !measurement.height.is_finite() || measurement.height <= 0. {
                    put(self.vm.heap_mut(), s, S::Fault, Value::bool(true))?;
                    changed = true;
                    continue;
                }
                let index = self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32;
                let epoch = int(self.vm.heap(), s, S::Epoch)?;
                let measured = self.vm.heap().sequence_get(e, EPOCH)?.as_int() == Some(epoch);
                let old = if measured {
                    stage::number(self.vm.heap().sequence_get(e, HEIGHT)?)?
                } else {
                    0.
                };
                if old != measurement.height || !measured {
                    let tree = get(self.vm.heap(), s, S::Tree)?;
                    let count = count(self.vm.heap(), s)?;
                    add(
                        self.vm.heap_mut(),
                        tree,
                        count,
                        index,
                        epoch,
                        i64::from(!measured),
                        measurement.height - old,
                    )?;
                    self.vm
                        .heap_mut()
                        .set_field(e, HEIGHT, Value::float(measurement.height))?;
                    self.vm.heap_mut().set_field(e, EPOCH, Value::int(epoch))?;
                    changed = true;
                }
            }
        }
        changed |= refresh_estimate(self.vm.heap_mut(), s)?;
        // Refill before unknown rows become visible. Half a viewport is the
        // low-water mark; construction restores a full viewport on both sides.
        // The hysteresis keeps small scrolls from rerunning the window observer.
        let reserve = (event.visible_end - event.visible_start) * 0.5;
        let first = locate(self.vm.heap(), s, (event.visible_start - reserve).max(0.))?;
        let last = locate(self.vm.heap(), s, (event.visible_end + reserve).max(0.))?;
        if count(self.vm.heap(), s)? > 0
            && (first..=last).any(|index| !view.rows.iter().any(|r| r.index == index as usize))
        {
            changed = true;
        }
        let pins = get(self.vm.heap(), s, S::Pinned)?;
        let same = self.vm.heap().sequence_len(pins)? as usize == retained.len()
            && retained.iter().enumerate().all(|(i, v)| {
                self.vm
                    .heap()
                    .sequence_get(pins, i as u32)
                    .is_ok_and(|x| x.same_bits(*v))
            });
        if !same {
            let pins = self.vm.heap_mut().alloc_array(&retained)?;
            put(self.vm.heap_mut(), s, S::Pinned, pins)?;
            changed = true;
        }
        put(
            self.vm.heap_mut(),
            s,
            S::MeasuredRevision,
            Value::int(event.revision as i64),
        )?;
        if changed {
            self.virtual_target_invalidate(s)?;
            if !self.virtual_target_position(s)? {
                self.virtual_anchor(s)?;
            }
        }
        if changed || moved {
            let request = int(self.vm.heap(), s, S::Request)? + 1;
            put(self.vm.heap_mut(), s, S::Request, Value::int(request))?;
            if changed {
                let demand = get(self.vm.heap(), s, S::Demand)?;
                observable::publish(self.vm.heap_mut(), demand, 0, Value::int(request))?;
            }
        }
        let armed = if !changed {
            self.virtual_target_arm(s)?
        } else {
            false
        };
        Ok(changed || armed)
    }
}

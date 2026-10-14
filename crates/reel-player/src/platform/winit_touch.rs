//! Native handles stay here; UI receives only portable pointer events.
use reel_ui::{core::*, dpi::PhysicalPosition};
use winit::event::{DeviceId, Touch, TouchPhase};

#[derive(Default)]
pub struct TouchInput {
    contacts: Vec<(DeviceId, u64, PointerId)>,
}

impl TouchInput {
    /// Convert a native contact. Unknown moves/ends after cancellation are ignored.
    /// IDs are unique among active contacts, including contacts on other devices.
    pub fn event(
        &mut self,
        touch: Touch,
        scale_factor: f64,
        modifiers: Modifiers,
    ) -> Option<PointerEvent> {
        let existing = self
            .contacts
            .iter()
            .position(|(device, id, _)| *device == touch.device_id && *id == touch.id);
        let index = match (touch.phase, existing) {
            (TouchPhase::Started, None) => {
                // Native IDs may be zero or u64::MAX and need not be globally unique.
                let id = (2..)
                    .map(|n| PointerId::new(n).unwrap())
                    .find(|id| self.contacts.iter().all(|(_, _, current)| current != id))?;
                self.contacts.push((touch.device_id, touch.id, id));
                self.contacts.len() - 1
            }
            (TouchPhase::Started, Some(_)) | (_, None) => return None,
            (_, Some(index)) => index,
        };
        let pointer = PointerInfo {
            pointer_id: Some(self.contacts[index].2),
            persistent_device_id: None,
            pointer_type: PointerType::Touch,
        };
        let terminal = matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled);
        let state = PointerState {
            position: PhysicalPosition::new(touch.location.x, touch.location.y),
            scale_factor,
            modifiers,
            pressure: if terminal {
                0.
            } else {
                touch.force.map(|f| f.normalized() as f32).unwrap_or(0.5)
            },
            ..Default::default()
        };
        if terminal {
            self.contacts.swap_remove(index);
        }
        Some(match touch.phase {
            TouchPhase::Started => PointerEvent::Down(PointerButtonEvent {
                pointer,
                state,
                button: None,
            }),
            TouchPhase::Moved => PointerEvent::Move(PointerUpdate {
                pointer,
                current: state,
                coalesced: Vec::new(),
                predicted: Vec::new(),
            }),
            TouchPhase::Ended => PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: None,
            }),
            TouchPhase::Cancelled => PointerEvent::Cancel(pointer),
        })
    }

    /// Call together with PresentationBridge::cancel_pointer_inputs on focus loss/suspension.
    pub fn clear(&mut self) {
        self.contacts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_contacts_preserve_position_pressure_identity_and_cancellation() {
        let mut input = TouchInput::default();
        let native = |phase, id| Touch {
            device_id: DeviceId::dummy(),
            phase,
            id,
            location: winit::dpi::PhysicalPosition::new(120., 80.),
            force: Some(winit::event::Force::Normalized(0.75)),
        };
        let first = input
            .event(native(TouchPhase::Started, 0), 2., Modifiers::SHIFT)
            .unwrap();
        let other = input
            .event(
                native(TouchPhase::Started, u64::MAX),
                2.,
                Modifiers::empty(),
            )
            .unwrap();
        let first_info = pointer_info(&first);
        let other_info = pointer_info(&other);
        assert_ne!(first_info, other_info);
        assert_ne!(first_info.pointer_id, Some(PointerId::PRIMARY));
        let PointerEvent::Down(e) = first else {
            panic!("touch down")
        };
        assert_eq!(
            e.state.logical_position(),
            reel_ui::dpi::LogicalPosition::new(60., 40.)
        );
        assert_eq!(e.state.pressure, 0.75);
        assert_eq!(e.state.modifiers, Modifiers::SHIFT);
        assert_eq!(e.button, None);
        let moved = input
            .event(native(TouchPhase::Moved, 0), 2., Modifiers::empty())
            .unwrap();
        assert_eq!(pointer_info(&moved), first_info);
        let cancelled = input
            .event(
                native(TouchPhase::Cancelled, u64::MAX),
                2.,
                Modifiers::empty(),
            )
            .unwrap();
        assert!(matches!(cancelled, PointerEvent::Cancel(p) if p == other_info));
        assert_eq!(input.contacts.len(), 1);
        let ended = input
            .event(native(TouchPhase::Ended, 0), 2., Modifiers::empty())
            .unwrap();
        assert!(
            matches!(ended, PointerEvent::Up(ref e) if e.pointer == first_info && e.state.pressure == 0.)
        );
        assert!(input.contacts.is_empty());
        input
            .event(native(TouchPhase::Started, 7), 1., Modifiers::empty())
            .unwrap();
        input.clear();
        assert!(
            input
                .event(native(TouchPhase::Moved, 7), 1., Modifiers::empty())
                .is_none()
        );
        assert!(
            input
                .event(native(TouchPhase::Ended, 7), 1., Modifiers::empty())
                .is_none()
        );
        assert!(
            input
                .event(native(TouchPhase::Started, 7), 1., Modifiers::empty())
                .is_some()
        );
    }
}

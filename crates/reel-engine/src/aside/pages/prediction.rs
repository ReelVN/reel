//! Select resource hints from existing page destinations without running a factory.
use super::*;
use aside_vm::HeapHandle;
use reel_ui::stage::{NodeId, NodeInteraction};

#[derive(Clone, Copy)]
struct Destination {
    stack: NodeId,
    factory: FunctionId,
}

#[derive(Clone, Copy)]
struct Interest {
    control: NodeId,
    destination: Option<Destination>,
}

struct Selection {
    destination: Destination,
    task: HeapHandle,
}

#[derive(Default)]
pub(in crate::aside) struct NavigationPrediction {
    focus: Option<Interest>,
    hover: Option<Interest>,
    selection: Option<Selection>,
}

impl SessionVm {
    pub(in crate::aside) fn observe_page_navigation(&mut self, event: &NodeInteraction) {
        if self.visual_plan.is_none()
            || self.stage.page_stacks.is_empty()
            || !self
                .stage
                .scene
                .node(event.node)
                .is_some_and(|node| matches!(node.kind, NodeKind::Radio { tabs: Some(_), .. }))
        {
            return;
        }
        let states = [
            event.state.focus_visible && event.state.focused,
            event.state.hover,
        ];
        let interests = [self.page_navigation.focus, self.page_navigation.hover];
        let matching = |interest: Interest| interest.control == event.node;
        let entering = interests.into_iter().zip(states).any(|(interest, active)| {
            active && !event.state.disabled && !interest.is_some_and(matching)
        });
        let candidate = entering.then(|| Interest {
            control: event.node,
            destination: self.tab_destination(event.node).ok().flatten(),
        });
        for (interest, active) in [
            &mut self.page_navigation.focus,
            &mut self.page_navigation.hover,
        ]
        .into_iter()
        .zip(states)
        {
            if active && !event.state.disabled {
                if let Some(candidate) = candidate {
                    *interest = Some(candidate);
                }
            } else if interest.is_some_and(matching) {
                *interest = None;
            }
        }
    }

    fn tab_destination(&self, control: NodeId) -> Result<Option<Destination>, AdapterError> {
        let h = self.vm.heap();
        let option = self.stage.value(h, control)?;
        let tab_bar = h.host_payload(option, 2)?;
        // A custom callback owns its navigation decision. A direct assignment
        // to the shared selected value identifies the normal native target.
        if field(h, tab_bar, TabField::OnSelect.index() as u32)?.tag() != ValueTag::Null {
            return Ok(None);
        }
        let (source, writable) = h.property_source(PropertyId {
            host: tab_bar,
            field: TabField::Value.index() as u32,
        })?;
        if !writable {
            return Ok(None);
        }
        let modal = self.stage.modal_scope(h)?;
        if !self.stage.mounted_in_scope(h, control, modal)? {
            return Ok(None);
        }
        let key = h
            .table_entry(
                h.host_fields(option)?,
                stage::controls::RadioField::Option.index() as u32,
            )?
            .1;
        let mut destination = None;
        for &stack in &self.stage.page_stacks {
            let root = self.stage.value(h, stack)?;
            let (selected, writable) = h.property_source(PropertyId {
                host: root,
                field: PageField::Value.index() as u32,
            })?;
            if !writable
                || source.field != selected.field
                || !source.host.same_bits(selected.host)
                || !self.stage.mounted_in_scope(h, stack, modal)?
            {
                continue;
            }
            let s = state(h, root)?;
            let Some(entry) = find(h, get(h, s, S::Sorted)?, key, self.read_limit)? else {
                continue;
            };
            if destination.is_some() {
                return Ok(None);
            }
            destination = Some((stack, entry));
        }
        let Some((stack, entry)) = destination else {
            return Ok(None);
        };
        if get(h, entry, E::Node)?.tag() != ValueTag::Null {
            return Ok(None);
        }
        let factory = self
            .vm
            .plain_closure_function(get(h, entry, E::Build)?)
            .ok();
        Ok(factory.map(|factory| Destination { stack, factory }))
    }

    pub(super) fn remember_page_selection(&mut self, root: Value, entry: Value, task: Value) {
        if self.visual_plan.is_none() {
            return;
        }
        let h = self.vm.heap();
        let Ok(stack) = self.stage.id(h, root) else {
            return;
        };
        if !self.navigation_node_live(stack) {
            return;
        }
        let pending = (|| {
            if get(h, entry, E::Node).ok()?.tag() != ValueTag::Null {
                return None;
            }
            let factory = self
                .vm
                .plain_closure_function(get(h, entry, E::Build).ok()?)
                .ok()?;
            Some(Selection {
                destination: Destination { stack, factory },
                task: h.handle(task).ok()?,
            })
        })();
        self.page_navigation.selection = pending;
    }

    pub(in crate::aside) fn forget_page_selection(&mut self, result: RootToken) {
        if let Some(selection) = &self.page_navigation.selection {
            let h = self.vm.heap();
            if h.root_value(result).and_then(|task| h.handle(task)).ok() == Some(selection.task) {
                self.page_navigation.selection = None;
            }
        }
    }

    /// The active page factory selected by a real request or native tab intent.
    pub(in crate::aside) fn focused_page_factory(&self) -> Option<FunctionId> {
        if let Some(selection) = &self.page_navigation.selection
            && self.navigation_node_live(selection.destination.stack)
        {
            return Some(selection.destination.factory);
        }
        [self.page_navigation.focus, self.page_navigation.hover]
            .into_iter()
            .flatten()
            .find_map(|interest| {
                let destination = interest.destination?;
                (self.navigation_node_live(interest.control)
                    && self.navigation_node_live(destination.stack))
                .then_some(destination.factory)
            })
    }

    fn navigation_node_live(&self, mut id: NodeId) -> bool {
        while let Some(node) = self.stage.scene.node(id) {
            if !node.props.visible || !node.props.enabled {
                return false;
            }
            let Some(parent) = node.parent else {
                return self.stage.scene.root() == Some(id);
            };
            id = parent;
        }
        false
    }
}

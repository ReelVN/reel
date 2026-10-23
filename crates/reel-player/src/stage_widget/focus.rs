//! Stable authored focus locations across host widget reconstruction.

use super::StageView;
use reel_ui::{
    app::{PresentedGeometryView, RenderRoot},
    core::WidgetId,
    stage::NodeId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FocusPath {
    nodes: Vec<usize>,
    widgets: Vec<usize>,
}

impl StageView {
    pub(crate) fn capture_focus_path(
        &self,
        root: &RenderRoot,
        geometry: &PresentedGeometryView,
        focused: WidgetId,
        widget_for_node: impl Fn(NodeId) -> Option<WidgetId>,
    ) -> Option<FocusPath> {
        let mut ancestors = Vec::new();
        let mut record = geometry.get(focused)?;
        loop {
            ancestors.push(record.id);
            let Some(parent) = record.parent else { break };
            record = &geometry.records()[parent];
        }
        let mut nearest = None;
        let mut pending = vec![(self.root.as_deref()?, Vec::new())];
        while let Some((node, path)) = pending.pop() {
            if let Some(widget) = widget_for_node(node.node.id)
                && let Some(distance) = ancestors.iter().position(|&id| id == widget)
                && nearest.as_ref().is_none_or(|(best, _)| distance < *best)
            {
                nearest = Some((distance, path.clone()));
                if distance == 0 {
                    break;
                }
            }
            for (index, child) in node.children.iter().enumerate() {
                let mut path = path.clone();
                path.push(index);
                pending.push((child, path));
            }
        }
        let (distance, nodes) = nearest?;
        let widgets = (1..=distance)
            .rev()
            .map(|index| {
                root.get_widget(ancestors[index])?
                    .children()
                    .iter()
                    .position(|child| child.id() == ancestors[index - 1])
            })
            .collect::<Option<Vec<_>>>()?;
        Some(FocusPath { nodes, widgets })
    }

    pub(crate) fn resolve_focus_path(
        &self,
        root: &RenderRoot,
        path: &FocusPath,
        widget_for_node: impl Fn(NodeId) -> Option<WidgetId>,
    ) -> Option<WidgetId> {
        let mut node = self.root.as_deref()?;
        for &index in &path.nodes {
            node = node.children.get(index)?;
        }
        let mut widget = widget_for_node(node.node.id)?;
        for &index in &path.widgets {
            widget = root.get_widget(widget)?.children().get(index)?.id();
        }
        Some(widget)
    }
}

//! Physical popup views share the logical stage's nodes and asset ownership.
use super::*;

#[derive(Clone)]
pub(crate) struct PopupSource {
    pub anchor: NodeId,
    pub(super) root: Arc<SceneNode>,
}
impl PopupSource {
    pub fn same_subtree(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.root, &other.root)
    }
    pub fn id(&self) -> NodeId {
        self.root.node.id
    }
    pub fn properties(&self) -> &NodeProps {
        &self.root.node.props
    }
    pub fn popup(&self) -> &reel_ui::stage::PopupView {
        let NodeKind::Popup(popup) = &self.root.node.kind else {
            unreachable!()
        };
        popup
    }
    pub fn view(&self, parent: &StageView) -> StageView {
        StageView {
            revision: parent.revision,
            button_revision: parent.button_revision,
            presentation_request: parent.presentation_request,
            feedback: parent.feedback,
            identity: parent.identity.clone(),
            root: Some(self.root.clone()),
            popups: None,
            image_index: Arc::default(),
            resources: Arc::from([]),
        }
    }
}

pub(super) fn sources(
    root: Option<&Arc<SceneNode>>,
    previous: Option<&StageView>,
    has_popups: bool,
) -> Option<Arc<[PopupSource]>> {
    if !has_popups {
        return None;
    }
    if let Some(previous) = previous
        && previous
            .root
            .as_ref()
            .zip(root)
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    {
        return previous.popups.clone();
    }
    fn collect(node: &Arc<SceneNode>, parent: Option<NodeId>, output: &mut Vec<PopupSource>) {
        if !node.popup_branch()
            || !node.node.props.visible
            || !node.node.props.enabled
            || matches!(node.node.kind, NodeKind::Cursor)
        {
            return;
        }
        if matches!(node.node.kind, NodeKind::Popup(_)) {
            let Some(anchor) = parent else {
                return;
            };
            output.push(PopupSource {
                anchor,
                root: node.clone(),
            });
        }
        for child in node.children.iter() {
            collect(child, Some(node.node.id), output);
        }
    }
    let mut collected = Vec::new();
    if let Some(root) = root {
        collect(root, None, &mut collected);
    }
    (!collected.is_empty()).then(|| Arc::from(collected))
}
impl StageView {
    pub(super) fn refresh_popups(&mut self) {
        self.popups = sources(
            self.root.as_ref(),
            None,
            self.root.as_ref().is_some_and(|root| root.popup_branch()),
        );
    }
    pub fn popups(&self) -> &[PopupSource] {
        self.popups.as_deref().unwrap_or_default()
    }
}

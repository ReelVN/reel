//! Portable window request transport and accepted pointer geometry for hosts.
use super::{AsideSessionUi, Error};
use reel_core::window::WindowInfo;
use reel_engine::aside::window::{WindowReply, WindowRequest, WindowRequestId};
use reel_ui::{bridge::PresentationBridge, core::WidgetId, stage::NodeId};
use std::sync::Arc;

impl AsideSessionUi {
    /// Publishes the host's current window state, independently of story saves.
    pub fn set_window_info(&mut self, info: Arc<WindowInfo>) {
        self.vm.set_window_info(info);
    }

    /// Claims the next live window request for the platform adapter.
    pub fn take_window_request(&mut self) -> Result<Option<WindowRequest>, Error> {
        self.vm.take_window_request().map_err(Into::into)
    }

    /// Completes a claimed request and wakes its ordinary script continuation.
    pub fn complete_window_request(
        &mut self,
        id: WindowRequestId,
        result: Result<WindowReply, String>,
    ) -> Result<bool, Error> {
        self.vm
            .complete_window_request(id, result)
            .map_err(Into::into)
    }

    /// Maps an authorized stage destination to accepted client-area coordinates.
    pub fn pointer_destination(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
        owner: NodeId,
        position: [f64; 2],
    ) -> Result<Option<[f64; 2]>, Error> {
        Ok(if self.vm.pointer_owner_permits_input(owner)? {
            self.ui.pointer_destination(bridge, root, owner, position)
        } else {
            None
        })
    }

    /// Publishes the pointer through the accepted stage-to-window transform.
    pub fn publish_pointer_position(
        &mut self,
        bridge: &PresentationBridge,
        root: WidgetId,
    ) -> Result<bool, Error> {
        self.vm
            .set_pointer_position(self.ui.pointer_position(bridge, root))
            .map_err(Into::into)
    }
}

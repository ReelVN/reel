//! Compose portable window requests with independently selected window/render adapters.
use super::*;
use reel_engine::aside::window::{WindowCommand, WindowReply};

impl<B> Player<B>
where
    B: RenderBackend + CaptureBackend<Error = <B as RenderBackend>::Error>,
{
    pub(crate) fn publish_input_hints(&mut self) -> Result<(), Error> {
        if let Some(PlayerSession::Aside { story, .. }) = &mut self.session {
            if !story.capturing()
                && story
                    .driver_mut()?
                    .adapter_mut()
                    .set_input_hints(self.input_hints.hints())?
            {
                story.wake_input();
            }
        }
        Ok(())
    }
    pub(crate) fn publish_pointer_info(&mut self) -> Result<(), Error> {
        self.publish_input_hints()?;
        if let Some(PlayerSession::Aside { story, .. }) = &mut self.session {
            if story.capturing() {
                return Ok(());
            }
            let position = story
                .driver()
                .ui()
                .pointer_position(&self.presentation.bridge, self.root_id);
            if story
                .driver_mut()?
                .adapter_mut()
                .set_pointer_position(position)?
            {
                story.wake_input();
            }
        }
        Ok(())
    }
    pub(crate) fn publish_window_info(&mut self, host: &mut impl Host) -> Result<(), Error> {
        let mut info = host.window_info();
        info.vsync = self.backend.vsync().unwrap_or(info.vsync);
        info.capabilities.vsync = self.backend.vsync().is_some();
        if *self.window_state != info {
            self.window_state = Arc::new(info);
        }
        if let Some(PlayerSession::Aside { story, .. }) = &mut self.session {
            story
                .driver_mut()?
                .adapter_mut()
                .set_window_info(self.window_state.clone());
        }
        Ok(())
    }
    pub(crate) fn complete_window_change(
        &mut self,
        result: Result<Arc<reel_core::window::WindowInfo>, String>,
    ) -> Result<(), Error> {
        let Some(id) = self.window_request.take() else {
            return Ok(());
        };
        if let Some(PlayerSession::Aside { story, .. }) = &mut self.session
            && story
                .driver_mut()?
                .adapter_mut()
                .complete_window_request(id, result.map(WindowReply::Configured))?
        {
            story.wake_input();
        }
        Ok(())
    }
    pub(crate) fn maintain_window_requests(&mut self, host: &mut impl Host) -> Result<(), Error> {
        if self.window_request.is_some() || self.closing || self.presentation.is_suspended() {
            return Ok(());
        }
        let Some(PlayerSession::Aside { story, .. }) = &mut self.session else {
            return Ok(());
        };
        if story.capturing() {
            return Ok(());
        }
        let Some(request) = story.driver_mut()?.adapter_mut().take_window_request()? else {
            return Ok(());
        };
        let configuration = match request.command {
            WindowCommand::Configure(configuration) => configuration,
            WindowCommand::Focus { target } => {
                let focused = story
                    .driver()
                    .adapter()
                    .focus_target_permits_input(target)?
                    && story.driver().ui().request_focus(
                        &mut self.presentation.bridge,
                        self.root_id,
                        target,
                    );
                if story
                    .driver_mut()?
                    .adapter_mut()
                    .complete_window_request(request.id, Ok(WindowReply::Focused(focused)))?
                {
                    story.wake_input();
                }
                if focused {
                    story
                        .driver_mut()?
                        .sync_input(&mut self.presentation.bridge, self.root_id)?;
                    self.invalidate();
                }
                return Ok(());
            }
            WindowCommand::MovePointer { position, owner } => {
                let destination = if story
                    .driver()
                    .adapter()
                    .pointer_owner_permits_input(owner)?
                {
                    story.driver().ui().pointer_destination(
                        &self.presentation.bridge,
                        self.root_id,
                        owner,
                        position,
                    )
                } else {
                    None
                };
                let metrics = host.display_metrics();
                let [width, height] = metrics
                    .physical_size
                    .map(|v| f64::from(v) / metrics.scale_factor);
                let destination = destination.filter(|[x, y]| {
                    x.is_finite()
                        && y.is_finite()
                        && *x >= 0.
                        && *y >= 0.
                        && *x < width
                        && *y < height
                });
                let result = match destination {
                    Some(position) => host.move_pointer(position),
                    None => Ok(false),
                };
                let moved = matches!(result, Ok(true));
                if story
                    .driver_mut()?
                    .adapter_mut()
                    .complete_window_request(request.id, result.map(WindowReply::PointerMoved))?
                {
                    story.wake_input();
                }
                if moved {
                    let [x, y] = destination.unwrap();
                    let scale = host.display_metrics().scale_factor;
                    self.pointer = reel_ui::dpi::PhysicalPosition::new(x * scale, y * scale);
                    self.pointer_event(PointerKind::Move)?;
                    self.publish_pointer_info()?;
                }
                return Ok(());
            }
        };
        self.window_request = Some(request.id);
        if configuration.width.is_some()
            || configuration.height.is_some()
            || configuration.fullscreen.is_some()
        {
            self.fit_initial_canvas = false;
        }
        let result = (|| -> Result<bool, String> {
            if let Some(vsync) = configuration.vsync {
                self.backend
                    .set_vsync(vsync)
                    .map_err(|error| error.to_string())?
                    .ok_or("presentation synchronization cannot be changed by this backend")?;
            }
            host.configure_window(configuration)
        })();
        match result {
            Ok(true) => {
                let actual = host.display_metrics().physical_size;
                if actual != [self.size.width, self.size.height] {
                    self.resize(host, host.display_metrics())?;
                } else {
                    self.publish_window_info(host)?;
                    self.complete_window_change(Ok(self.window_state.clone()))?;
                }
                self.invalidate();
            }
            Ok(false) => {
                self.publish_window_info(host)?;
            }
            Err(error) => {
                self.publish_window_info(host)?;
                self.complete_window_change(Err(error))?;
            }
        }
        Ok(())
    }
}

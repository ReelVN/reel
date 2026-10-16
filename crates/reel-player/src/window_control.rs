//! Compose portable window requests with independently selected window/render adapters.
use super::*;

impl State {
    pub(super) fn publish_window_info(&mut self) -> Result<(), Error> {
        let mut info = self.host.window_info();
        info.vsync = self.backend.vsync_enabled();
        info.capabilities.vsync = self.backend.vsync().is_some();
        if *self.window_state != info {
            self.window_state = Arc::new(info);
        }
        if let Some(WindowSession::Aside { story, .. }) = &mut self.session {
            story
                .driver_mut()?
                .adapter_mut()
                .set_window_info(self.window_state.clone());
        }
        Ok(())
    }
    pub(crate) fn monitor_changed(&mut self) -> Result<(), Error> {
        self.publish_window_info()
    }

    pub(super) fn complete_window_change(
        &mut self,
        result: Result<Arc<reel_core::window::WindowInfo>, String>,
    ) -> Result<(), Error> {
        let Some(id) = self.window_request.take() else {
            return Ok(());
        };
        if let Some(WindowSession::Aside { story, .. }) = &mut self.session
            && story
                .driver_mut()?
                .adapter_mut()
                .complete_window_request(id, result)?
        {
            story.wake_input();
        }
        Ok(())
    }
    pub(super) fn maintain_window_requests(&mut self) -> Result<(), Error> {
        if self.window_request.is_some() || self.closing || self.is_suspended() {
            return Ok(());
        }
        let Some(WindowSession::Aside { story, .. }) = &mut self.session else {
            return Ok(());
        };
        if story.capturing() {
            return Ok(());
        }
        let Some(request) = story.driver_mut()?.adapter_mut().take_window_request()? else {
            return Ok(());
        };
        self.window_request = Some(request.id);
        if request.configuration.width.is_some()
            || request.configuration.height.is_some()
            || request.configuration.fullscreen.is_some()
        {
            self.fit_initial_canvas = false;
        }
        let result = (|| -> Result<bool, String> {
            if let Some(vsync) = request.configuration.vsync {
                self.backend
                    .set_vsync(vsync)
                    .map_err(|error| error.to_string())?
                    .ok_or("presentation synchronization cannot be changed by this backend")?;
            }
            self.host.configure_window(request.configuration)
        })();
        match result {
            Ok(true) => {
                let actual = self.window.inner_size();
                if [actual.width, actual.height] != [self.size.width, self.size.height] {
                    self.resize(actual)?;
                } else {
                    self.publish_window_info()?;
                    self.complete_window_change(Ok(self.window_state.clone()))?;
                }
                self.invalidate();
            }
            Ok(false) => {
                self.publish_window_info()?;
            }
            Err(error) => {
                self.publish_window_info()?;
                self.complete_window_change(Err(error))?;
            }
        }
        Ok(())
    }
}

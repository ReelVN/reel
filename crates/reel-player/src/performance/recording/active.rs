//! Request ownership and diagnostic guards; the recorder owns one active run.
use super::Live;
use crate::performance::{Request, StatusState};
use std::io;

pub(super) struct Active {
    pub request: Request,
    pub live: Option<Live>,
    _wall: Option<reel_core::profile::WallRecording>,
    previous_vm: Option<bool>,
    previous_atlas: Option<bool>,
}
impl Active {
    pub(super) fn start(request: Request) -> io::Result<Self> {
        request.validate().map_err(io::Error::other)?;
        std::fs::create_dir_all(&request.output)?;
        let previous_vm = aside_vm::executor::set_profiling_enabled(request.vm_timing);
        let previous_atlas = request.source_atlas.map(|enabled| {
            let previous = reel_render::profiling::shared_source_atlas();
            reel_render::profiling::set_shared_source_atlas(enabled);
            previous
        });
        let mut active = Self {
            request,
            live: None,
            _wall: None,
            previous_vm: Some(previous_vm),
            previous_atlas,
        };
        let result = (|| {
            active._wall =
                Some(reel_core::profile::WallRecording::start().ok_or_else(|| {
                    io::Error::other("performance wall recording already active")
                })?);
            active.live = Some(Live::start()?);
            active.request.write_status(StatusState::Running, None)
        })();
        if let Err(error) = result {
            let _ = active
                .request
                .write_status(StatusState::Error, Some(&error.to_string()));
            return Err(error);
        }
        Ok(active)
    }

    pub(super) fn stop(&mut self) {
        drop(self.live.take());
        drop(self._wall.take());
        if let Some(previous) = self.previous_vm.take() {
            aside_vm::executor::set_profiling_enabled(previous);
        }
        if let Some(previous) = self.previous_atlas.take() {
            reel_render::profiling::set_shared_source_atlas(previous);
        }
    }
}
impl Drop for Active {
    fn drop(&mut self) {
        self.stop();
    }
}

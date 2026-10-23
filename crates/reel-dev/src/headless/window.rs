//! Virtual client-window state using the production request and input contracts.
use reel_core::window::{WindowCapabilities, WindowInfo, WindowSize};
use reel_engine::aside::window::{WindowCommand, WindowReply};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{
    bridge::PresentationBridge,
    core::{
        PointerEvent, PointerId, PointerInfo, PointerState, PointerType, PointerUpdate, WidgetId,
    },
    dpi::PhysicalPosition,
};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct VirtualWindow {
    info: WindowInfo,
    display: WindowSize,
    configured: bool,
}

impl VirtualWindow {
    pub fn new() -> Self {
        let size = WindowSize {
            width: 640.,
            height: 420.,
        };
        Self {
            info: WindowInfo {
                size,
                windowed_size: size,
                scale_factor: 1.,
                vsync: true,
                capabilities: WindowCapabilities {
                    resize: true,
                    fullscreen: true,
                    vsync: true,
                    pointer_move: true,
                },
                resolutions: Arc::from([size]),
                ..Default::default()
            },
            display: size,
            configured: false,
        }
    }

    /// Fits the initial client area to the authored canvas until explicitly resized.
    pub fn publish(&mut self, driver: &mut AsideSessionUi) {
        if !self.configured
            && let Some([width, height]) = driver.adapter().game_settings().canvas_size
        {
            let size = WindowSize {
                width: f64::from(width),
                height: f64::from(height),
            };
            if self.display != size {
                self.info.size = size;
                self.info.windowed_size = size;
                self.info.resolutions = Arc::from([size]);
                self.display = size;
            }
        }
        self.install(driver);
    }

    /// Installs saved host state before a branch's own canvas is restored.
    pub fn install(&self, driver: &mut AsideSessionUi) {
        if driver.adapter().window_info().as_ref() != &self.info {
            driver.set_window_info(Arc::new(self.info.clone()));
        }
    }

    pub fn size(&self) -> [u32; 2] {
        [
            self.info.size.width.round() as u32,
            self.info.size.height.round() as u32,
        ]
    }

    pub fn fingerprint(&self) -> [u64; 8] {
        [
            self.info.size.width.to_bits(),
            self.info.size.height.to_bits(),
            self.info.windowed_size.width.to_bits(),
            self.info.windowed_size.height.to_bits(),
            self.display.width.to_bits(),
            self.display.height.to_bits(),
            u64::from(self.info.fullscreen),
            u64::from(self.info.vsync) | (u64::from(self.configured) << 1),
        ]
    }

    /// Applies one request through the same ownership and geometry checks as a window host.
    pub fn drive(
        &mut self,
        driver: &mut AsideSessionUi,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<bool, String> {
        self.publish(driver);
        let Some(request) = driver.take_window_request().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let reply = match request.command {
            WindowCommand::Configure(config) => {
                self.configured |= config.width.is_some()
                    || config.height.is_some()
                    || config.fullscreen.is_some();
                if let Some(width) = config.width {
                    self.info.windowed_size.width = width;
                }
                if let Some(height) = config.height {
                    self.info.windowed_size.height = height;
                }
                if let Some(fullscreen) = config.fullscreen {
                    self.info.fullscreen = fullscreen;
                }
                if let Some(vsync) = config.vsync {
                    self.info.vsync = vsync;
                }
                self.info.size = if self.info.fullscreen {
                    self.display
                } else {
                    self.info.windowed_size
                };
                let info = Arc::new(self.info.clone());
                driver.set_window_info(info.clone());
                WindowReply::Configured(info)
            }
            WindowCommand::MovePointer { position, owner } => {
                let destination = driver
                    .pointer_destination(bridge, root, owner, position)
                    .map_err(|e| e.to_string())?
                    .filter(|[x, y]| {
                        *x >= 0.
                            && *y >= 0.
                            && *x < self.info.size.width
                            && *y < self.info.size.height
                    });
                if let Some([x, y]) = destination {
                    driver
                        .handle_pointer(
                            bridge,
                            PointerEvent::Move(PointerUpdate {
                                pointer: PointerInfo {
                                    pointer_id: Some(PointerId::PRIMARY),
                                    persistent_device_id: None,
                                    pointer_type: PointerType::Mouse,
                                },
                                current: PointerState {
                                    position: PhysicalPosition::new(x, y),
                                    ..Default::default()
                                },
                                coalesced: vec![],
                                predicted: vec![],
                            }),
                        )
                        .map_err(|e| e.to_string())?;
                    driver
                        .publish_pointer_position(bridge, root)
                        .map_err(|e| e.to_string())?;
                }
                WindowReply::PointerMoved(destination.is_some())
            }
        };
        driver
            .complete_window_request(request.id, Ok(reply))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{RunOptions, RunOutcome, run};

    #[test]
    fn virtual_window_changes_use_the_ordinary_waitable_protocol() {
        let story = reel_engine::aside::source::compile_story(
            r#"
            fn main() {
                game.configure(width: 320, height: 240);
                stage.replace(stack());
                let resized = await window.configure(width: 640, height: 480, vsync: false);
                if resized.width != 640 || resized.height != 480 || resized.vsync { throw "resize result"; }
                let full = await window.configure(fullscreen: true);
                if !full.fullscreen || full.width != 320 || full.height != 240 { throw "fullscreen result"; }
                let restored = await window.configure(fullscreen: false);
                if restored.width != 640 || restored.height != 480 { throw "windowed result"; }
            }
            "#,
            "virtual_window", 1, "main",
        ).unwrap();
        let resources = tempfile::tempdir().unwrap();
        let report = run(story, resources.path(), &RunOptions::default(), &[]).unwrap();
        assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
    }
}

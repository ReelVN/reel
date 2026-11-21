use crate::{
    compile::{self, RunResult},
    host::CanvasHost,
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::CompiledStory},
};
use reel_player::{
    aside_session::AsideSessionUi,
    assets::Assets,
    hal::DisplayMetrics,
    player::{Player, PlayerOptions, PointerKind},
};
use reel_render::{
    effect::{ShaderTarget, prepare_shader},
    gpu::{DeviceGeneration, Gpu, GpuLimits, WgpuBackend},
};
use reel_ui::{
    core::{PointerButton, ScrollDelta, TextEvent, keyboard},
    dpi::PhysicalPosition,
    input::InputEvent,
    resources::EffectShaderFormat,
};
use std::{collections::BTreeMap, sync::Arc};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

const DEFAULT_FONT: &str = "fonts/body.ttf";

/// An independent Reel session mounted on a browser-owned canvas.
#[wasm_bindgen]
pub struct WebPreview {
    canvas: HtmlCanvasElement,
    instance: wgpu::Instance,
    gpu: Gpu,
    locale: String,
    assets: BTreeMap<String, Arc<[u8]>>,
    player: Option<Player<WgpuBackend>>,
    host: CanvasHost,
    pointer: [f64; 2],
}

#[wasm_bindgen]
impl WebPreview {
    /// Request a WebGPU device for the supplied page canvas.
    pub async fn create(canvas: HtmlCanvasElement, locale: String) -> Result<WebPreview, JsValue> {
        console_error_panic_hook::set_once();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(js_error)?;
        let gpu = Gpu::request(
            &instance,
            Some(&surface),
            DeviceGeneration(1),
            GpuLimits::default(),
        )
        .await
        .map_err(js_error)?;
        let host = CanvasHost::new(DisplayMetrics {
            physical_size: [canvas.width().max(1), canvas.height().max(1)],
            scale_factor: 1.,
        });
        Ok(Self {
            canvas,
            instance,
            gpu,
            locale,
            assets: BTreeMap::new(),
            player: None,
            host,
            pointer: [0., 0.],
        })
    }

    /// Supply a project-relative asset shared by subsequent runs on this canvas.
    #[wasm_bindgen(js_name = setAsset)]
    pub fn set_asset(&mut self, path: String, bytes: Vec<u8>) -> Result<(), JsValue> {
        reel_package::archive::validate_path(&path).map_err(js_error)?;
        self.assets.insert(path, bytes.into());
        Ok(())
    }

    /// Compile and start a new run. Source errors are returned as diagnostic JSON.
    pub fn run(&mut self, source: &str) -> String {
        let result = match compile::snippet(source) {
            Err(result) => result,
            Ok(compiled) => match self.start(compiled) {
                Ok(()) => RunResult::Running,
                Err(error) => RunResult::RuntimeError {
                    message: error.to_string(),
                },
            },
        };
        serde_json::to_string(&result).expect("preview results serialize as JSON")
    }

    /// Advance ready work and present a changed view at the browser's frame boundary.
    pub fn frame(&mut self) -> Result<bool, JsValue> {
        let Some(player) = &mut self.player else {
            return Ok(false);
        };
        player.poll_completions().map_err(js_error)?;
        player.tick(&mut self.host).map_err(js_error)?;
        if player.close_complete() || self.host.exiting {
            self.player = None;
            return Ok(false);
        }
        if self.host.take_redraw() || player.needs_redraw() {
            player.render(&mut self.host).map_err(js_error)?;
        } else {
            player
                .prepare_initial_frame(&mut self.host)
                .map_err(js_error)?;
        }
        Ok(true)
    }

    /// Size the drawing buffer in physical pixels while retaining the CSS pixel ratio.
    pub fn resize(&mut self, width: u32, height: u32, scale: f64) -> Result<(), JsValue> {
        let physical_size = [width.max(1), height.max(1)];
        if self.host.metrics.physical_size != physical_size {
            self.canvas.set_width(physical_size[0]);
            self.canvas.set_height(physical_size[1]);
        }
        self.host.metrics = DisplayMetrics {
            physical_size,
            scale_factor: scale,
        };
        if let Some(player) = &mut self.player {
            player
                .resize(
                    &mut self.host,
                    DisplayMetrics {
                        physical_size,
                        scale_factor: scale,
                    },
                )
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// Forward canvas-local pointer movement in CSS pixels.
    #[wasm_bindgen(js_name = pointerMove)]
    pub fn pointer_move(&mut self, x: f64, y: f64) -> Result<(), JsValue> {
        let scale = self.host.metrics.scale_factor;
        self.pointer = [x * scale, y * scale];
        self.send_pointer(PointerKind::Move)
    }

    /// Forward a browser pointer button edge.
    #[wasm_bindgen(js_name = pointerButton)]
    pub fn pointer_button(&mut self, button: u8, down: bool) -> Result<(), JsValue> {
        let button = match button {
            0 => PointerButton::Primary,
            1 => PointerButton::Auxiliary,
            2 => PointerButton::Secondary,
            _ => return Ok(()),
        };
        self.send_pointer(if down {
            PointerKind::Down(button)
        } else {
            PointerKind::Up(button)
        })
    }

    #[wasm_bindgen(js_name = pointerCancel)]
    pub fn pointer_cancel(&mut self) -> Result<(), JsValue> {
        self.send_pointer(PointerKind::Cancel)
    }

    pub fn scroll(&mut self, x: f64, y: f64) -> Result<(), JsValue> {
        let scale = self.host.metrics.scale_factor;
        self.send_pointer(PointerKind::Scroll(ScrollDelta::PixelDelta(
            PhysicalPosition::new(-x * scale, -y * scale),
        )))
    }

    /// Forward a browser keyboard edge to the engine's normal input routing.
    pub fn key(&mut self, code: &str, key: &str, down: bool) -> Result<(), JsValue> {
        if let Some(player) = &mut self.player {
            let event = keyboard::KeyboardEvent {
                code: code.parse().unwrap_or(keyboard::Code::Unidentified),
                key: key
                    .parse()
                    .unwrap_or(keyboard::Key::Named(keyboard::NamedKey::Unidentified)),
                state: if down {
                    keyboard::KeyState::Down
                } else {
                    keyboard::KeyState::Up
                },
                ..Default::default()
            };
            player
                .text_event(TextEvent::Keyboard(event))
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// Clear held controls when keyboard focus leaves the canvas.
    #[wasm_bindgen(js_name = cancelInput)]
    pub fn cancel_input(&mut self) -> Result<(), JsValue> {
        if let Some(player) = &mut self.player {
            player
                .text_event(TextEvent::Input(InputEvent::Cancel))
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// Reflect the browser window's focus in the preview's reading controls.
    pub fn focus(&mut self, focused: bool) -> Result<(), JsValue> {
        if let Some(player) = &mut self.player {
            player
                .text_event(TextEvent::WindowFocusChange(focused))
                .map_err(js_error)?;
        }
        Ok(())
    }
}

impl WebPreview {
    fn start(&mut self, compiled: CompiledStory) -> Result<(), Box<dyn std::error::Error>> {
        let options = PlayerOptions {
            resource_target: "web".into(),
            ..Default::default()
        };
        let mut files = self.assets.clone();
        for definition in compiled.effects.definitions() {
            let shader = prepare_shader(definition.id, &definition.program, ShaderTarget::Wgsl)?;
            files.insert(
                EffectShaderFormat::Wgsl.asset_path(definition.id),
                shader.bytes.into(),
            );
        }
        let assets =
            Assets::memory(options.assets.clone(), files).map_err(|error| error.message)?;
        let mut limits = VmLimits::default();
        limits.heap.max_bytes = 80 << 20;
        limits.slots.initial_capacity = 4096;
        limits.slots.max_capacity = 65_536;
        limits.max_tasks = 256;
        let session = Session::new(SessionId(1), Epoch(1))?;
        let mut driver = AsideSessionUi::from_compiled(session, compiled, &self.locale, limits)?
            .with_assets(assets);
        if self.assets.contains_key(DEFAULT_FONT) {
            driver = driver.with_default_fonts(vec![DEFAULT_FONT.into()]);
        }
        // Keep the device across edits, and release the current canvas surface
        // before configuring it for a fresh player.
        self.player = None;
        self.host = CanvasHost::new(self.host.metrics);
        let surface = self
            .instance
            .create_surface(wgpu::SurfaceTarget::Canvas(self.canvas.clone()))?;
        let backend = WgpuBackend::new(self.gpu.clone(), surface);
        self.player = Some(Player::new(driver, backend, &mut self.host, &options)?);
        Ok(())
    }

    fn send_pointer(&mut self, kind: PointerKind) -> Result<(), JsValue> {
        if let Some(player) = &mut self.player {
            player.pointer_input(self.pointer, kind).map_err(js_error)?;
        }
        Ok(())
    }
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    wasm_bindgen::JsError::new(&error.to_string()).into()
}

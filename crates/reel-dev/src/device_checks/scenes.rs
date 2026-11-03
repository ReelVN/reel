use super::{Error, Result, SIZE, r01_fixture};
#[path = "masks.rs"]
mod masks;
#[path = "scenes/regional_instances.rs"]
mod regional_instances;
#[path = "scenes/text.rs"]
mod text;
pub const TEXT_REVEAL_FRAMES: usize = text::LINE.len();
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
    texture::{TextureDescriptor, TextureFormat, TextureLayout, TexturePayload},
};
use reel_ui::{
    core::{PaintEffect, PaintImageMask},
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, EffectShaderDescriptor, ResourceHandle, ResourceRegistry},
    stage::effect::{EffectInstance, EffectProgram, EffectValue, lower_mask},
};
use std::sync::Arc;

pub struct Assets {
    pool: BudgetPool,
    registry: ResourceRegistry,
    handles: Vec<ResourceHandle>,
    pub versions: Vec<ResourceVersion>,
    text: Option<text::TextScene>,
    static_mask: Option<(ResourceHandle, Arc<EffectProgram>)>,
    regional_instances: Option<regional_instances::RegionalInstances>,
}
fn error(error: impl std::fmt::Debug) -> Error {
    format!("{error:?}").into()
}
impl Assets {
    pub fn new() -> Result<Self> {
        Ok(Self {
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 4096,
                    max_bytes: 64 << 20,
                },
            ),
            registry: ResourceRegistry::new(32, 16 << 20).map_err(error)?,
            handles: Vec::new(),
            versions: Vec::new(),
            text: None,
            static_mask: None,
            regional_instances: None,
        })
    }
    pub fn retire(&mut self) {
        self.text = None;
        self.static_mask = None;
        self.regional_instances = None;
        for handle in self.handles.drain(..) {
            self.registry.retire(handle);
            self.registry.release(handle);
        }
        self.versions.clear();
    }
    pub fn usage(&self) -> reel_core::budget::BudgetUsage {
        self.pool.usage()
    }
    fn texture(&mut self, case: &str, index: usize) -> Result<ResourceHandle> {
        if let Some(handle) = self.handles.first() {
            return Ok(*handle);
        }
        let version = ResourceVersion {
            key: AssetKey(format!("probe-{case}-{index}")),
            version: ContentVersion([index as u8; 32]),
        };
        let size = [256, 256];
        let handle = if case.starts_with("bc1")
            || case.starts_with("bc3")
            || case == "resources" && !index.is_multiple_of(2)
        {
            let format = if case.starts_with("bc1") {
                TextureFormat::Bc1Rgb
            } else {
                TextureFormat::Bc3Rgba
            };
            let mut bytes = Vec::new();
            for block in 0..64 * 64 {
                if format == TextureFormat::Bc3Rgba {
                    bytes.extend_from_slice(&[255, 64, 0, 0, 0, 0, 0, 0]);
                }
                bytes.extend_from_slice(&[0, 248, 224, 7, block as u8, 85, 170, 255]);
            }
            let length = bytes.len() as u64;
            let owned = OwnedBytes::new(bytes, self.pool.reserve(length).map_err(error)?)
                .map_err(|e| error(e.reason))?;
            self.registry
                .register_texture(
                    version.clone(),
                    TexturePayload::new(
                        TextureDescriptor {
                            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                            mip_count: 1,
                            format,
                            layout: if case.ends_with("_region") {
                                TextureLayout {
                                    logical_size: size,
                                    content_size: [240, 248],
                                    storage_size: size,
                                }
                            } else {
                                TextureLayout::identity(size)
                            },
                            source_encoding: ColorEncoding::Linear,
                        },
                        owned,
                    )?,
                )
                .map_err(error)?
        } else {
            let mut bytes = Vec::with_capacity(256 * 256 * 4);
            for y in 0..256 {
                for x in 0..256 {
                    bytes.extend_from_slice(&[x as u8, y as u8, (x + y + index) as u8, 255]);
                }
            }
            let bytes_len = bytes.len() as u64;
            self.registry
                .register_image(
                    version.clone(),
                    OwnedBytes::new(bytes, self.pool.reserve(bytes_len).map_err(error)?)
                        .map_err(|e| error(e.reason))?,
                    size,
                    ImageFormat::Rgba8,
                    ImageAlphaType::Alpha,
                    ColorEncoding::Srgb,
                )
                .map_err(error)?
        };
        self.handles.push(handle);
        self.versions.push(version);
        Ok(handle)
    }
    fn static_mask(&mut self) -> Result<(ResourceHandle, Arc<EffectProgram>)> {
        if let Some((handle, program)) = &self.static_mask {
            return Ok((*handle, program.clone()));
        }
        let payload = include_bytes!(concat!(env!("OUT_DIR"), "/static-mask.reel-effect"));
        let (descriptor, range) =
            EffectShaderDescriptor::decode_payload(payload, 4 << 20).map_err(error)?;
        let raw = &payload[range];
        let version = ResourceVersion {
            key: AssetKey(descriptor.format.asset_path(descriptor.definition)),
            version: ContentVersion(descriptor.shader_key),
        };
        let owned = OwnedBytes::new(
            raw.to_vec(),
            self.pool.reserve(raw.len() as u64).map_err(error)?,
        )
        .map_err(|error| self::error(error.reason))?;
        let handle = self
            .registry
            .register_effect_shader(version.clone(), descriptor, owned)
            .map_err(error)?;
        let program = lower_mask(masks::program("mask_dag").map_err(error)?.as_ref())
            .ok_or_else(|| error("the shared DAG is not statically lowerable"))?;
        self.handles.push(handle);
        self.versions.push(version);
        self.static_mask = Some((handle, program.clone()));
        Ok((handle, program))
    }
    pub fn frame(&mut self, case: &str, index: usize) -> Result<RenderFrame> {
        if case == "regional_instances" {
            if self.regional_instances.is_none() {
                self.regional_instances = Some(regional_instances::RegionalInstances::new(
                    &mut self.registry,
                    &self.pool,
                    &mut self.handles,
                    &mut self.versions,
                )?);
            }
            return self
                .regional_instances
                .as_ref()
                .unwrap()
                .frame(index, &self.pool);
        }
        if matches!(case, "text" | "text_plain" | "text_styled") {
            if self.text.is_none() {
                self.text = Some(text::TextScene::new(
                    &mut self.registry,
                    &self.pool,
                    &mut self.handles,
                    &mut self.versions,
                    case,
                )?);
            }
            return self.text.as_mut().unwrap().frame(
                index,
                &self.registry,
                &self.handles,
                &self.pool,
            );
        }
        let mut scene = Scene::new();
        if case == "offscreen" {
            // Repeated small overlapping groups and clip scopes use real replay,
            // scratch reuse and the shared bounds optimization.
            let groups = 16;
            let depth = 1;
            for group in 0..groups {
                for _ in 0..depth {
                    scene.push_group(Group {
                        composite: Composite {
                            alpha: 0.65,
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                }
                let transform = Affine::translate((
                    30.5 + (group % 4) as f64 * 230.,
                    30.5 + (group / 4) as f64 * 125.,
                )) * Affine::rotate(0.08);
                for x in [0., 40.] {
                    fill(&mut scene, Rect::new(x, 0., x + 110., 70.), transform);
                }
                for _ in 0..depth {
                    scene.pop_group();
                }
            }
        } else if case != "empty" {
            fill(&mut scene, Rect::new(0., 0., 960., 544.), Affine::IDENTITY);
        }
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        if !matches!(case, "empty" | "solid" | "offscreen") {
            let handle = self.texture(case, index)?;
            let lease = self.registry.acquire(handle).map_err(error)?;
            let progress = 0.1 + 0.8 * (index % 8) as f32 / 7.;
            if case == "mask_static" {
                let (shader, program) = self.static_mask()?;
                widget.mask_effect = Some(PaintEffect {
                    instance: EffectInstance::with_textures(
                        masks::STATIC_DEFINITION,
                        program,
                        [progress, 0., 0.125, 0.]
                            .map(|value: f32| EffectValue::Float(value.to_bits()))
                            .to_vec(),
                        vec!["probe-mask".into()],
                    )
                    .map_err(error)?,
                    shader: self.registry.acquire(shader).map_err(error)?,
                    images: vec![lease],
                    bounds: Rect::new(0., 0., 960., 544.),
                    coverage_invert: false,
                    resolution_scale: 1.,
                });
            } else if case.starts_with("mask_") {
                widget.mask = Some(PaintImageMask {
                    program: masks::program(case).map_err(error)?,
                    images: vec![lease].into(),
                    bounds: Rect::new(0., 0., 960., 544.),
                    parameters: [progress, 0., 0.125, 0., 0.],
                });
            } else {
                widget.resource_draws.push((
                    lease,
                    Rect::new(0., 0., 256., 256.),
                    Affine::scale_non_uniform(960. / 256., 544. / 256.),
                    Default::default(),
                ));
            }
        }
        let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
        let source = bridge.record(FrameId(index as u64 + 1)).map_err(error)?;
        let snapshot = self
            .registry
            .freeze(&self.handles, &self.pool)
            .map_err(error)?;
        let mut build = FrameBuild::new(
            source,
            snapshot,
            FrameInfo {
                logical_size: [960., 544.],
                physical_size: SIZE,
            },
            r01_fixture::limits(),
            &self.pool,
        )
        .map_err(error)?;
        let mut budget = r01_fixture::step_budget();
        loop {
            let progress = build.step(budget).map_err(error)?;
            if progress.complete {
                break;
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
        build.finish().map_err(error)
    }
}

pub(super) fn check_regional_pixels(pixels: &[u8]) -> Result {
    regional_instances::check_pixels(pixels)
}

fn fill(scene: &mut Scene, rect: Rect, transform: Affine) {
    scene.draw(Draw::Fill {
        transform,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::from_rgba8(70, 150, 220, 220)),
        brush_transform: None,
        shape: Geometry::Rect(rect),
        composite: Composite::default(),
    });
}

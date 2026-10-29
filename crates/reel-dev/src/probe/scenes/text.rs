//! Persistent generic VN workload: BC1 scenery, two alpha BC3 portraits and
//! a real shaped line. Hidden suffixes enter GPU preparation only when revealed.
use super::*;
use crate::device::fixture::StageGroup;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::PresentationBridge,
    core::{DefaultProperties, StyleProperty, TextOutline, TextShadow, Widget, WidgetId},
    dpi::PhysicalSize,
    kurbo::{Point, Size, Vec2},
    peniko::Blob,
    properties::ContentColor,
    resources::ResourceData,
    widgets::{Image, Label},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

pub(super) const LINE: &str =
    "A quiet evening brings new stories, familiar voices, and time to listen.";
const FONT: &[u8] = include_bytes!(
    "../../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
);

pub(super) struct TextScene {
    bridge: PresentationBridge,
    label: WidgetId,
    visible: usize,
}
impl TextScene {
    pub fn new(
        registry: &mut ResourceRegistry,
        pool: &BudgetPool,
        handles: &mut Vec<ResourceHandle>,
        versions: &mut Vec<ResourceVersion>,
        case: &str,
    ) -> Result<Self> {
        let font = Blob::from(FONT.to_vec());
        let version = version(200);
        let handle = registry
            .register(
                version.clone(),
                ResourceData::Font(font.clone()),
                pool.reserve(font.len() as u64).map_err(error)?,
            )
            .map_err(error)?;
        handles.push(handle);
        versions.push(version);
        let scenery = case == "text";
        let styled = case != "text_plain";
        let mut background_scene = Scene::new();
        if !scenery {
            background_scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::from_rgb8(20, 34, 55)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 960., 544.)),
                composite: Default::default(),
            });
        }
        let mut root = r01_fixture::RecordWidget::new(background_scene);
        root.clip = false;
        root.inner.clip = Rect::new(0., 0., 960., 544.);
        if scenery {
            let background = texture(registry, pool, handles, versions, 201, false)?;
            let left = texture(registry, pool, handles, versions, 202, true)?;
            let right = texture(registry, pool, handles, versions, 203, true)?;
            root.inner.add(
                Image::from_resource(registry.acquire(background).map_err(error)?).prepare(),
                Point::ZERO,
                Size::new(960., 544.),
            );
            root.inner.add(
                Image::from_resource(registry.acquire(left).map_err(error)?).prepare(),
                Point::new(145., 20.),
                Size::new(320., 427.),
            );
            root.inner.add(
                Image::from_resource(registry.acquire(right).map_err(error)?).prepare(),
                Point::new(495., 30.),
                Size::new(320., 427.),
            );
        }

        let mut panel_scene = Scene::new();
        if scenery {
            panel_scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::from_rgba8(12, 18, 32, 230)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 864., 104.)),
                composite: Default::default(),
            });
        }
        let mut panel = r01_fixture::RecordWidget::new(panel_scene);
        panel.clip = false;
        // Text shares a stable isolated source with its dialogue background.
        panel.opacity = if scenery { 0.96 } else { 1. };
        panel.inner = StageGroup::new(
            Rc::new(RefCell::new(Vec::new())),
            1.,
            Rect::new(0., 0., 864., 104.),
        );
        let label = panel.inner.add(
            Label::new(LINE)
                .with_visible_bytes(0..0)
                .with_style(StyleProperty::FontSize(22.))
                .with_outline(styled.then_some(TextOutline {
                    width: 1.5,
                    color: Color::from_rgba8(0, 0, 0, 220),
                }))
                .with_shadow(styled.then_some(TextShadow {
                    offset: Vec2::new(1., 1.),
                    color: Color::from_rgba8(0, 0, 0, 160),
                }))
                .prepare()
                .with_props(ContentColor::new(Color::from_rgb8(255, 255, 255))),
            Point::new(20., 29.),
            Size::new(824., 48.),
        );
        root.inner.add(
            panel.prepare(),
            Point::new(48., 420.),
            Size::new(864., 104.),
        );
        let root = RenderRoot::new(
            root.prepare(),
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(SIZE[0], SIZE[1]),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        Ok(Self {
            bridge: r01_fixture::bridge(root),
            label,
            visible: 0,
        })
    }

    pub fn frame(
        &mut self,
        index: usize,
        registry: &ResourceRegistry,
        handles: &[ResourceHandle],
        pool: &BudgetPool,
    ) -> Result<RenderFrame> {
        let visible = index.saturating_add(1).min(LINE.len());
        if visible != self.visible {
            self.bridge.edit_widget(self.label, |mut label| {
                Label::set_visible_bytes(&mut label.downcast::<Label>(), 0..visible)
            });
            self.visible = visible;
        }
        let source = self
            .bridge
            .record(FrameId(index as u64 + 1))
            .map_err(error)?;
        let snapshot = registry.freeze(handles, pool).map_err(error)?;
        let mut build = FrameBuild::new(
            source,
            snapshot,
            FrameInfo {
                logical_size: [960., 544.],
                physical_size: SIZE,
            },
            r01_fixture::limits(),
            pool,
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

fn version(index: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey(format!("probe-vn-{index}")),
        version: ContentVersion([index; 32]),
    }
}

fn texture(
    registry: &mut ResourceRegistry,
    pool: &BudgetPool,
    handles: &mut Vec<ResourceHandle>,
    versions: &mut Vec<ResourceVersion>,
    index: u8,
    portrait: bool,
) -> Result<ResourceHandle> {
    let storage_size = if portrait { [512, 512] } else { [1024, 1024] };
    let content_size = if portrait { [384, 512] } else { SIZE };
    let format = if portrait {
        TextureFormat::Bc3Rgba
    } else {
        TextureFormat::Bc1Rgb
    };
    let length = format
        .byte_len(storage_size)
        .ok_or("VN texture dimensions")?;
    let storage = pool.reserve(length).map_err(error)?;
    let mut bytes = Vec::with_capacity(length as usize);
    for by in 0..storage_size[1] / 4 {
        for bx in 0..storage_size[0] / 4 {
            let x = f64::from((bx * 4 + 2).min(content_size[0] - 1));
            let y = f64::from((by * 4 + 2).min(content_size[1] - 1));
            let (alpha, rgb) = if portrait {
                let head = 1. - ((x - 192.) / 76.).powi(2) - ((y - 106.) / 86.).powi(2);
                let body = (1. - ((x - 192.) / (88. + y * 0.15)).abs()).min((y - 150.) / 24.);
                let alpha = (head.max(body) * 4.).clamp(0., 1.);
                let alpha = if bx * 4 >= content_size[0] { 0. } else { alpha };
                let color = if index.is_multiple_of(2) {
                    [160., 90., 56.]
                } else {
                    [55., 105., 165.]
                };
                (
                    (alpha * 255.) as u8,
                    color.map(|component| (component * alpha) as u8),
                )
            } else {
                (
                    255,
                    [
                        35 + (x / 32.) as u8,
                        45 + (y / 16.) as u8,
                        68 + ((x + y) / 48.) as u8,
                    ],
                )
            };
            if portrait {
                bytes.extend_from_slice(&[alpha, alpha, 0, 0, 0, 0, 0, 0]);
            }
            let color = ((u16::from(rgb[0]) >> 3) << 11)
                | ((u16::from(rgb[1]) >> 2) << 5)
                | (u16::from(rgb[2]) >> 3);
            bytes.extend_from_slice(&color.to_le_bytes());
            bytes.extend_from_slice(&color.saturating_sub(1).to_le_bytes());
            bytes.extend_from_slice(&[0; 4]);
        }
    }
    let bytes = OwnedBytes::new(bytes, storage).map_err(|e| error(e.reason))?;
    let payload = TexturePayload::new(
        TextureDescriptor {
            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
            mip_count: 1,
            format,
            layout: TextureLayout {
                logical_size: content_size,
                content_size,
                storage_size,
            },
            source_encoding: ColorEncoding::Linear,
        },
        bytes,
    )?;
    let version = version(index);
    let handle = registry
        .register_texture(version.clone(), payload)
        .map_err(error)?;
    handles.push(handle);
    versions.push(version);
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vn_fixture_retains_widgets_resources_and_only_grows_visible_text() {
        let mut assets = Assets::new().unwrap();
        let first = assets.frame("text", 0).unwrap();
        let glyphs = |frame: &RenderFrame| {
            frame
                .commands()
                .iter()
                .filter_map(|command| match &command.op {
                    reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                        Some(run.glyphs.len())
                    }
                    _ => None,
                })
                .sum::<usize>()
        };
        let owner = |frame: &RenderFrame| {
            frame
                .commands()
                .iter()
                .find_map(|command| {
                    matches!(
                        command.op,
                        reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(_))
                    )
                    .then_some(command.origin.at.owner)
                })
                .unwrap()
        };
        let first_glyphs = glyphs(&first);
        assert!(first_glyphs > 0);
        let stable_owner = owner(&first);
        let mut full_glyphs = 0;
        for index in 1..LINE.len() + 8 {
            let frame = assets.frame("text", index).unwrap();
            assert_eq!(owner(&frame), stable_owner);
            assert_eq!(assets.versions.len(), 4);
            if index == LINE.len() - 1 {
                full_glyphs = glyphs(&frame);
            }
            if index >= LINE.len() {
                assert_eq!(glyphs(&frame), full_glyphs);
            }
        }
        assert!(full_glyphs > first_glyphs);
        assert_eq!(first.resources().leases().iter().filter(|lease| matches!(lease.view(), reel_ui::resources::ResourceView::Texture { descriptor, .. } if descriptor.format == TextureFormat::Bc1Rgb)).count(), 1);
        assert_eq!(first.resources().leases().iter().filter(|lease| matches!(lease.view(), reel_ui::resources::ResourceView::Texture { descriptor, .. } if descriptor.format == TextureFormat::Bc3Rgba)).count(), 2);
        println!(
            "vn_text reveal_frames={} warm_frames=8 first_glyphs={first_glyphs} full_glyphs={full_glyphs} cpu_pool={:?}",
            LINE.len(),
            assets.usage()
        );
    }
}

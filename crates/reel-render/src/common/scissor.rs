use reel_ui::{
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
};
#[derive(Clone, Copy)]
pub(crate) struct Scissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Scissor {
    pub fn full(size: [u32; 2]) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size[0],
            height: size[1],
        }
    }
    pub fn empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn intersect(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width)
                .min(other.x + other.width)
                .saturating_sub(x),
            height: (self.y + self.height)
                .min(other.y + other.height)
                .saturating_sub(y),
        }
    }
    pub fn from_clip(clip: &Clip, outer: Affine, size: [u32; 2]) -> Option<Self> {
        let Clip::Fill {
            transform,
            shape: Geometry::Rect(rect),
            ..
        } = clip
        else {
            return None;
        };
        let transform = outer * *transform;
        let [_, b, c, _, _, _] = transform.as_coeffs();
        if b != 0. || c != 0. {
            return None;
        }
        let Rect { x0, y0, x1, y1 } = transform.transform_rect_bbox(*rect);
        if [x0, y0, x1, y1]
            .iter()
            .any(|n| !n.is_finite() || n.fract() != 0.)
        {
            return None;
        }
        let x0 = x0.clamp(0., size[0] as f64) as u32;
        let x1 = x1.clamp(0., size[0] as f64) as u32;
        let y0 = y0.clamp(0., size[1] as f64) as u32;
        let y1 = y1.clamp(0., size[1] as f64) as u32;
        Some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
    #[cfg(feature = "wgpu")]
    pub fn apply(self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_scissor_rect(self.x, self.y, self.width, self.height);
    }
}

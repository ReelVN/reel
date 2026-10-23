//! Shared subtree blending, including modes outside the Imaging blend vocabulary.
use crate::{imaging, peniko};

/// How a completed subtree is combined with its backdrop in linear premultiplied RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlendMode {
    /// Preserve the standard Imaging blend and composition operation.
    Standard(peniko::BlendMode),
    /// Clamp backdrop RGB minus premultiplied source RGB to zero, preserving backdrop alpha.
    /// Group opacity and masks already scale the source before this operation.
    Subtract,
}
impl Default for BlendMode {
    fn default() -> Self {
        Self::Standard(peniko::BlendMode::default())
    }
}
impl From<peniko::BlendMode> for BlendMode {
    fn from(value: peniko::BlendMode) -> Self {
        Self::Standard(value)
    }
}
impl From<peniko::Mix> for BlendMode {
    fn from(value: peniko::Mix) -> Self {
        Self::Standard(value.into())
    }
}
impl BlendMode {
    /// Return the standard recording representation, or None when Imaging cannot express it.
    pub fn imaging(self) -> Option<peniko::BlendMode> {
        match self {
            Self::Standard(value) => Some(value),
            Self::Subtract => None,
        }
    }
}

/// The blend and opacity applied once to a completed frame group.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GroupComposite {
    /// Operation applied to the group result and its backdrop.
    pub blend: BlendMode,
    /// Group opacity from zero to one, applied once after subtree rendering.
    pub alpha: f32,
}
impl Default for GroupComposite {
    fn default() -> Self {
        imaging::Composite::default().into()
    }
}
impl From<imaging::Composite> for GroupComposite {
    fn from(value: imaging::Composite) -> Self {
        Self {
            blend: value.blend.into(),
            alpha: value.alpha,
        }
    }
}

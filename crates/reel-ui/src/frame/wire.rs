//! Snapshot adapters for portable drawing types supplied by Imaging.
use super::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

pub(super) fn origin() -> CommandOrigin {
    CommandOrigin {
        source: 0,
        local: LocalId::None,
        at: retained::error(BuildErrorKind::InvalidRecord).at,
    }
}

pub(super) mod widget {
    use super::*;
    pub fn serialize<S: Serializer>(value: &WidgetId, serializer: S) -> Result<S::Ok, S::Error> {
        value.to_raw().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<WidgetId, D::Error> {
        let raw = u64::deserialize(deserializer)?;
        if raw == 0 {
            return Err(serde::de::Error::custom("zero widget identity"));
        }
        Ok(WidgetId(raw.try_into().unwrap()))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Composite")]
pub(super) struct CompositeDef {
    pub blend: peniko::BlendMode,
    pub alpha: f32,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Geometry")]
pub(super) enum GeometryDef {
    Rect(Rect),
    RoundedRect(crate::kurbo::RoundedRect),
    Path(BezPath),
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Clip")]
pub(super) enum ClipDef {
    Fill {
        transform: Affine,
        #[serde(with = "GeometryDef")]
        shape: Geometry,
        fill_rule: peniko::Fill,
    },
    Stroke {
        transform: Affine,
        #[serde(with = "GeometryDef")]
        shape: Geometry,
        stroke: Stroke,
    },
}

pub(super) mod optional_clip {
    use super::*;
    #[derive(Serialize)]
    struct Ref<'a>(#[serde(with = "ClipDef")] &'a Clip);
    #[derive(Deserialize)]
    struct Owned(#[serde(with = "ClipDef")] Clip);
    pub fn serialize<S: Serializer>(
        value: &Option<Clip>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Ref).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Clip>, D::Error> {
        Ok(Option::<Owned>::deserialize(deserializer)?.map(|value| value.0))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Filter")]
enum FilterDef {
    Flood {
        color: peniko::Color,
    },
    Blur {
        std_deviation_x: f32,
        std_deviation_y: f32,
    },
    DropShadow {
        dx: f32,
        dy: f32,
        std_deviation_x: f32,
        std_deviation_y: f32,
        color: peniko::Color,
    },
    Offset {
        dx: f32,
        dy: f32,
    },
}

pub(super) mod filters {
    use super::*;
    #[derive(Serialize)]
    struct Ref<'a>(#[serde(with = "FilterDef")] &'a Filter);
    #[derive(Deserialize)]
    struct Owned(#[serde(with = "FilterDef")] Filter);
    pub fn serialize<S: Serializer>(value: &[Filter], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(value.len()))?;
        for filter in value {
            seq.serialize_element(&Ref(filter))?;
        }
        seq.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Filter>, D::Error> {
        Ok(Vec::<Owned>::deserialize(deserializer)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "record::Glyph")]
struct GlyphDef {
    id: u32,
    x: f32,
    y: f32,
}

pub(super) mod glyphs {
    use super::*;
    #[derive(Serialize)]
    struct Ref<'a>(#[serde(with = "GlyphDef")] &'a record::Glyph);
    #[derive(Deserialize)]
    struct Owned(#[serde(with = "GlyphDef")] record::Glyph);
    pub fn serialize<S: Serializer>(
        value: &[record::Glyph],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(value.len()))?;
        for glyph in value {
            seq.serialize_element(&Ref(glyph))?;
        }
        seq.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<record::Glyph>, D::Error> {
        Ok(Vec::<Owned>::deserialize(deserializer)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "imaging::BlurredRoundedRect")]
pub(super) struct BlurredRectDef {
    transform: Affine,
    rect: Rect,
    color: peniko::Color,
    radius: f64,
    std_dev: f64,
    #[serde(with = "CompositeDef")]
    composite: Composite,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "MaskMode")]
pub(super) enum MaskModeDef {
    Alpha,
    Luminance,
}

pub(super) mod mask_program {
    use super::*;
    use crate::stage::{MaskOp, MaskProgram};
    pub fn serialize<S: Serializer>(
        value: &Arc<MaskProgram>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (
            value.sources(),
            value.operations(),
            value.is_image_threshold(),
        )
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<MaskProgram>, D::Error> {
        let (sources, ops, threshold): (Vec<Arc<str>>, Vec<MaskOp>, bool) =
            Deserialize::deserialize(deserializer)?;
        if threshold {
            if sources.len() != 1 || sources[0].is_empty() {
                return Err(serde::de::Error::custom("invalid image mask"));
            }
            let program = MaskProgram::image(sources[0].clone());
            if program.operations() != ops {
                return Err(serde::de::Error::custom(
                    "invalid image threshold operations",
                ));
            }
            Ok(program)
        } else {
            MaskProgram::new(ops, sources).map_err(serde::de::Error::custom)
        }
    }
}

pub(super) mod version {
    use super::*;
    use reel_core::identity::{AssetKey, ContentVersion, ResourceVersion};
    pub fn serialize<S: Serializer>(
        value: &ResourceVersion,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        (&value.key.0, value.version.0).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ResourceVersion, D::Error> {
        let (key, version) = Deserialize::deserialize(deserializer)?;
        Ok(ResourceVersion {
            key: AssetKey(key),
            version: ContentVersion(version),
        })
    }
}

pub(super) mod yuv {
    use super::*;
    use reel_core::image::{
        Yuv420Image, Yuv420Layout, Yuv420Storage, YuvColor, YuvMatrix, YuvTransfer,
    };
    #[derive(Serialize, Deserialize)]
    #[serde(remote = "Yuv420Layout")]
    enum Layout {
        Planar,
        Nv12,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(remote = "YuvMatrix")]
    enum Matrix {
        Bt601,
        Bt709,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(remote = "YuvTransfer")]
    enum Transfer {
        Bt709,
        Srgb,
    }
    #[derive(Serialize, Deserialize)]
    #[serde(remote = "YuvColor")]
    struct Color {
        #[serde(with = "Matrix")]
        matrix: YuvMatrix,
        full_range: bool,
        #[serde(with = "Transfer")]
        transfer: YuvTransfer,
    }
    #[derive(Serialize)]
    struct Ref<'a> {
        size: [u32; 2],
        #[serde(with = "Color")]
        color: YuvColor,
        #[serde(with = "Layout")]
        layout: Yuv420Layout,
        row_strides: [u32; 3],
        planes: &'a [&'a [u8]],
    }
    #[derive(Deserialize)]
    struct Owned {
        size: [u32; 2],
        #[serde(with = "Color")]
        color: YuvColor,
        #[serde(with = "Layout")]
        layout: Yuv420Layout,
        row_strides: [u32; 3],
        planes: Vec<Vec<u8>>,
    }
    #[derive(Debug)]
    struct Planes(Vec<Vec<u8>>);
    impl Yuv420Storage for Planes {
        fn plane(&self, index: usize) -> &[u8] {
            &self.0[index]
        }
    }
    pub fn serialize<S: Serializer>(value: &Yuv420Image, serializer: S) -> Result<S::Ok, S::Error> {
        let mut planes: [&[u8]; 3] = [&[]; 3];
        for (i, plane) in planes.iter_mut().enumerate().take(value.plane_count()) {
            *plane = value.plane(i);
        }
        Ref {
            size: value.size(),
            color: value.color(),
            layout: value.layout(),
            row_strides: std::array::from_fn(|i| {
                if i < value.plane_count() {
                    value.row_stride(i)
                } else {
                    0
                }
            }),
            planes: &planes[..value.plane_count()],
        }
        .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Yuv420Image, D::Error> {
        let value = Owned::deserialize(deserializer)?;
        let expected = match value.layout {
            Yuv420Layout::Planar => 3,
            Yuv420Layout::Nv12 => 2,
        };
        if value.planes.len() != expected {
            return Err(serde::de::Error::custom(
                "invalid retained video plane count",
            ));
        }
        Yuv420Image::with_layout(
            value.size,
            value.color,
            value.layout,
            value.row_strides,
            Arc::new(Planes(value.planes)),
        )
        .map_err(|error| serde::de::Error::custom(format!("invalid retained video: {error:?}")))
    }
}

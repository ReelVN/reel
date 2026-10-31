//! Paint variants are created on first use and retain their native budget lease.
use super::{BudgetPool, GpuError, NAMES, Shader, create_program_with_vertex, native};
use std::cell::OnceCell;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    Direct(crate::common::mesh::DirectPaint),
    Bounded(crate::common::mesh::DirectPaint),
    Rgba,
    Coverage,
    Gradient,
    GlyphGradient,
    Appearance,
    Packed,
    Video,
    VideoAppearance,
    VideoDirectNv12,
    AppearanceConstant,
    PackedConstant,
    VideoConstant,
    AppearanceSimple {
        regional: bool,
        constant: bool,
        plain_adjust: bool,
    },
}
impl Kind {
    #[cfg(feature = "profiling")]
    pub(crate) fn diagnostic_label(self) -> &'static str {
        [
            "paint.rgba",
            "paint.bc1",
            "paint.bc3",
            "paint.regional",
            "paint.regional_bc1",
            "paint.regional_bc3",
            "paint.coverage_direct",
            "paint.solid",
            "paint.rgba_general",
            "paint.coverage",
            "paint.gradient",
            "paint.glyph_gradient",
            "paint.appearance",
            "paint.packed",
            "paint.video",
            "paint.video_appearance",
            "paint.rgba_bounded",
            "paint.bc1_bounded",
            "paint.bc3_bounded",
            "paint.solid_bounded",
            "paint.nv12_direct",
            "paint.appearance_constant",
            "paint.packed_constant",
            "paint.video_constant",
            "paint.appearance_direct",
            "paint.appearance_region",
            "paint.constant_direct",
            "paint.constant_region",
            "paint.rgba_region_bounded",
            "paint.bc1_region_bounded",
            "paint.bc3_region_bounded",
        ][if self.index() >= 28 {
            self.index() - 4
        } else {
            self.index()
        }]
    }
    fn index(self) -> usize {
        match self {
            Self::Direct(direct) => direct as usize,
            Self::Bounded(direct) => match direct {
                crate::common::mesh::DirectPaint::RegionalRgba => 32,
                crate::common::mesh::DirectPaint::RegionalBc1 => 33,
                crate::common::mesh::DirectPaint::RegionalBc3 => 34,
                _ => {
                    16 + if direct as usize == 7 {
                        3
                    } else {
                        direct as usize
                    }
                }
            },
            Self::Rgba => 8,
            Self::Coverage => 9,
            Self::Gradient => 10,
            Self::GlyphGradient => 11,
            Self::Appearance => 12,
            Self::Packed => 13,
            Self::Video => 14,
            Self::VideoAppearance => 15,
            Self::VideoDirectNv12 => 20,
            Self::AppearanceConstant => 21,
            Self::PackedConstant => 22,
            Self::VideoConstant => 23,
            Self::AppearanceSimple {
                regional,
                constant,
                plain_adjust,
            } => {
                24 + usize::from(regional)
                    + 2 * usize::from(constant)
                    + 4 * usize::from(plain_adjust)
            }
        }
    }
    fn code(self, clipped: bool) -> native::ShaderCode<'static> {
        macro_rules! pair {
            ($name:literal) => {
                if clipped {
                    shader_code!($name)
                } else {
                    native::ShaderCode::GxpBinary(include_bytes!(concat!(
                        env!("OUT_DIR"),
                        "/",
                        $name,
                        "_unclipped.gxp"
                    )))
                }
            };
        }
        match self.index() {
            0 => pair!("paint_direct"),
            1 => pair!("paint_direct_bc1"),
            2 => pair!("paint_direct_bc3"),
            3 => pair!("paint_regional"),
            4 => pair!("paint_regional_bc1"),
            5 => pair!("paint_regional_bc3"),
            6 => pair!("paint_direct_coverage"),
            7 => pair!("paint_solid"),
            8 => pair!("paint_rgba"),
            9 => pair!("paint_coverage"),
            10 => pair!("paint_gradient"),
            11 => pair!("paint_glyph_gradient"),
            12 => pair!("paint_appearance"),
            13 => pair!("paint_packed"),
            14 => pair!("paint_video"),
            15 => pair!("paint_video_appearance"),
            16 => pair!("paint_direct_bounded"),
            17 => pair!("paint_direct_bc1_bounded"),
            18 => pair!("paint_direct_bc3_bounded"),
            19 => pair!("paint_solid_bounded"),
            20 => pair!("paint_video_direct_nv12"),
            21 => pair!("paint_appearance_constant"),
            22 => pair!("paint_packed_constant"),
            23 => pair!("paint_video_constant"),
            24 => pair!("paint_appearance_direct"),
            25 => pair!("paint_appearance_region"),
            26 => pair!("paint_constant_direct"),
            27 => pair!("paint_constant_region"),
            28 => pair!("paint_adjust_direct"),
            29 => pair!("paint_adjust_region"),
            30 => pair!("paint_constant_plain_direct"),
            31 => pair!("paint_constant_plain_region"),
            32 => pair!("paint_regional_bounded"),
            33 => pair!("paint_regional_bc1_bounded"),
            34 => pair!("paint_regional_bc3_bounded"),
            _ => unreachable!(),
        }
    }
}
struct Entry {
    shader: Shader,
    _storage: super::super::ownership::NativeTicket,
}
pub(crate) struct PaintPrograms {
    entries: [OnceCell<Box<Entry>>; 140],
    budget: BudgetPool,
}
impl PaintPrograms {
    pub fn new(budget: &BudgetPool) -> Self {
        Self {
            entries: std::array::from_fn(|_| OnceCell::new()),
            budget: budget.clone(),
        }
    }
    pub fn get(
        &self,
        device: &super::super::ownership::Device,
        kind: Kind,
        clipped: bool,
        instanced: bool,
    ) -> Result<&Shader, GpuError> {
        let slot =
            &self.entries[kind.index() * 4 + usize::from(clipped) * 2 + usize::from(instanced)];
        if slot.get().is_none() {
            #[cfg(feature = "profiling")]
            eprintln!(
                "[reel-render] paint shader={kind:?} clipped={clipped} instanced={instanced}"
            );
            let storage = device.ticket(
                self.budget
                    .reserve(size_of::<Entry>() as u64)
                    .map_err(|error| GpuError::Device(error.message))?,
            );
            let program = create_program_with_vertex(
                device,
                if instanced && matches!(kind, Kind::Bounded(_)) {
                    shader_code!("vertex_instances_bounded")
                } else if instanced {
                    shader_code!("vertex_instances")
                } else if matches!(kind, Kind::Bounded(_)) {
                    shader_code!("vertex_bounded")
                } else {
                    shader_code!("vertex")
                },
                kind.code(clipped),
                clipped,
                instanced,
            )?;
            let uniforms = std::array::from_fn(|index| program.uniform(NAMES[index]));
            let entry = Box::new(Entry {
                shader: Shader { program, uniforms },
                _storage: storage,
            });
            assert!(slot.set(entry).is_ok(), "single-threaded program creation");
        }
        Ok(&slot.get().unwrap().shader)
    }
}

//! Explicit hardware-check selection shared by device startup and host tests.
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub(super) enum Check {
    All,
    Render,
    ClearSwap,
    Fence,
    Empty,
    Solid,
    Rgba,
    Bc1,
    Bc3,
    Bc1Region,
    Bc3Region,
    RegionalInstances,
    Text,
    TextPlain,
    TextStyled,
    Offscreen,
    MaskChain,
    MaskThreshold,
    MaskDag,
    MaskStatic,
    Resources,
    VideoColor,
    BlendColor,
    CompositeColor,
    CompositeReference,
    PresentColor,
    PaintRegion,
    EffectRegion,
}

impl Check {
    pub(super) fn name(self) -> String {
        self.to_possible_value().unwrap().get_name().to_owned()
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "reel-device-checks",
    about = "Validate native rendering contracts"
)]
pub(super) struct Options {
    /// Run a bounded hardware contract check; all excludes stored-reference cases.
    #[arg(long, value_enum, default_value = "all")]
    pub(super) case: Check,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_cases_have_a_bounded_selection() {
        assert_eq!(
            Options::try_parse_from(["checks"]).unwrap().case,
            Check::All
        );
        assert_eq!(
            Options::try_parse_from(["checks", "--case", "effect_region"])
                .unwrap()
                .case,
            Check::EffectRegion
        );
        for case in [
            "authored",
            "frozen",
            "diagnostic",
            "ablation",
            "matrix",
            "media",
        ] {
            assert!(Options::try_parse_from(["checks", "--case", case]).is_err());
        }
        for option in ["--frames", "effect-timing=serial", "frame-index=3"] {
            assert!(Options::try_parse_from(["checks", option]).is_err());
        }
    }
}

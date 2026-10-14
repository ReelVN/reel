//! Portable creator settings, independent of window and graphics adapters.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameSettings {
    pub title: String,
    pub display: DisplaySettings,
    pub font: Option<String>,
}
impl Default for GameSettings {
    fn default() -> Self {
        Self {
            title: "Reel".into(),
            display: DisplaySettings::default(),
            font: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaySettings {
    pub width: u32,
    pub height: u32,
    pub scale: DisplayScale,
}
impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            scale: DisplayScale::Contain,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayScale {
    Contain,
}

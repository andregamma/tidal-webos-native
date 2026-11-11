use crate::geom::Color;
use serde::Deserialize;
use std::path::Path;

/// Colours and a few metrics. Loaded from `assets/theme.json`; the simulator
/// reloads it on save so you can tune the look live.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub bg: Color,
    pub sidebar_bg: Color,
    pub surface: Color,
    pub surface_focus: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub card_radius: f32,
}

impl Default for Theme {
    fn default() -> Self {
        let h = |s: &str| Color::from_hex(s).unwrap();
        Theme {
            bg: h("#0b0d12"),
            sidebar_bg: h("#12151c"),
            surface: h("#1a1f2a"),
            surface_focus: h("#2a3142"),
            text: h("#f2f4f8"),
            text_dim: h("#8b93a7"),
            accent: h("#2ee6d0"),
            card_radius: 18.0,
        }
    }
}

impl Theme {
    pub fn from_json(s: &str) -> Result<Theme, serde_json::Error> {
        serde_json::from_str(s)
    }

    pub fn load(path: &Path) -> Result<Theme, String> {
        let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Theme::from_json(&s).map_err(|e| e.to_string())
    }
}

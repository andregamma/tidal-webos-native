//! Platform-independent core: state, navigation/focus, layout and animation.
//!
//! Nothing here touches SDL, OpenGL or the network. The UI is *immediate mode*:
//! every frame `App::view` emits a flat [`draw::DrawList`] that a renderer
//! (see `tidal-gfx`) turns into GPU draw calls. This keeps the whole UI
//! testable headlessly and identical between the desktop simulator and the TV.

pub mod anim;
pub mod app;
pub mod draw;
pub mod geom;
pub mod i18n;
pub mod images;
pub mod input;
pub mod model;
pub mod quality;
pub mod screens;
pub mod services;
pub mod text;
pub mod theme;
pub mod widgets;
pub mod ui;

/// Logical (design) resolution. Layout is authored in these units; the
/// renderer scales to whatever the real drawable size is.
pub const W: f32 = 1920.0;
pub const H: f32 = 1080.0;

/// Per-frame context handed to every `view`.
pub struct Cx<'a> {
    pub theme: &'a theme::Theme,
    pub m: &'a dyn text::TextMetrics,
    pub images: &'a dyn images::Images,
    /// Seconds since start (for pulses/skeletons).
    pub time: f32,
}

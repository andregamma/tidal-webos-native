//! OpenGL ES 2.0 renderer for `tidal-core` draw lists.
//!
//! One shader, one dynamic vertex buffer, one glyph atlas. Rounded rectangles
//! are SDF quads (anti-aliased, no geometry for corners); text is textured
//! quads from an A8 atlas. Draw calls only break at `Clip` commands.

mod atlas;
mod renderer;

pub use renderer::Renderer;

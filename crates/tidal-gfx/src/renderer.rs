use crate::atlas::Atlas;
use fontdue::{Font, FontSettings};
use glow::HasContext;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tidal_core::draw::{Cmd, DrawList};
use tidal_core::geom::{Color, Rect};
use tidal_core::images::{ImgState, Images};
use tidal_core::text::TextMetrics;

const VERT: &str = r#"#version 100
attribute vec2 a_pos;
attribute vec2 a_uv;
attribute vec2 a_local;
attribute vec3 a_rect;   // half-width, half-height, radius
attribute float a_mode;  // 0 = rounded rect (SDF), 1 = glyph
attribute vec4 a_color;
uniform vec2 u_view;
varying vec2 v_uv;
varying vec2 v_local;
varying vec3 v_rect;
varying float v_mode;
varying vec4 v_color;
void main() {
    v_uv = a_uv; v_local = a_local; v_rect = a_rect; v_mode = a_mode; v_color = a_color;
    gl_Position = vec4(a_pos.x / u_view.x * 2.0 - 1.0, 1.0 - a_pos.y / u_view.y * 2.0, 0.0, 1.0);
}
"#;

// highp where available: fp16 mediump visibly breaks SDF edges on some TV GPUs.
const FRAG: &str = r#"#version 100
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform sampler2D u_tex;
uniform float u_aa;      // one physical pixel, in logical units
varying vec2 v_uv;
varying vec2 v_local;
varying vec3 v_rect;
varying float v_mode;
varying vec4 v_color;
void main() {
    float a;
    vec3 rgb = v_color.rgb;
    if (v_mode > 1.5) {
        // Image texture with rounded corners from the same SDF mask. Covers are RGB (alpha reads
        // as 1); the brand logos are RGBA, so their own alpha shapes them.
        vec2 p = abs(v_local) - (v_rect.xy - vec2(v_rect.z));
        float d = length(max(p, 0.0)) + min(max(p.x, p.y), 0.0) - v_rect.z;
        vec4 tex = texture2D(u_tex, v_uv);
        a = clamp(0.5 - d / u_aa, 0.0, 1.0) * tex.a;
        rgb = tex.rgb;
    } else if (v_mode > 0.5) {
        a = texture2D(u_tex, v_uv).a;
    } else {
        vec2 p = abs(v_local) - (v_rect.xy - vec2(v_rect.z));
        float d = length(max(p, 0.0)) + min(max(p.x, p.y), 0.0) - v_rect.z;
        a = clamp(0.5 - d / u_aa, 0.0, 1.0);
    }
    gl_FragColor = vec4(rgb, v_color.a * a);
}
"#;

/// Cover textures kept on the GPU (least recently used are evicted). RGB, so ~0.3 MB at 320 px.
/// The budget is in pixels: at 4K the covers are twice as wide, so four times the memory.
const MAX_IMAGES: usize = 96;
const MAX_IMAGES_4K: usize = 32;

/// Glyph atlas edge, in texels. Text is rasterised per physical px, so a 4K drawable needs
/// roughly four times the area — 1024² would run out and reset mid-frame (visible as text
/// dropping for one frame).
const ATLAS_PX: u32 = 1024;
const ATLAS_PX_4K: u32 = 2048;

struct GpuImage {
    tex: glow::Texture,
    last_used: u64,
    w: u32,
    h: u32,
}

const FLOATS: usize = 14; // pos2 uv2 local2 rect3 mode1 color4
const STRIDE: i32 = (FLOATS * 4) as i32;

pub struct Renderer {
    gl: glow::Context,
    prog: glow::Program,
    vbo: glow::Buffer,
    tex: glow::Texture,
    u_view: Option<glow::UniformLocation>,
    u_aa: Option<glow::UniformLocation>,
    atlas: Atlas,
    font: Font,
    /// UI font first; a second font only for characters it lacks (see [`Renderer::glyph_font`]).
    fallback: Font,
    adv_cache: RefCell<HashMap<(char, u16), f32>>,
    verts: Vec<f32>,
    logical: (f32, f32),
    phys: (u32, u32),
    scale: f32,
    vp: (i32, i32, i32, i32),
    clip: Option<Rect>,
    pub draw_calls: u32,
    images: HashMap<u64, GpuImage>,
    pending: HashSet<u64>,
    failed: HashSet<u64>,
    frame: u64,
}

impl Renderer {
    /// `gl` must be a current GLES2+ context. `logical` is the design size. `fallback_bytes` is
    /// only used for characters `font_bytes` has no glyph for.
    pub fn new(
        gl: glow::Context,
        font_bytes: &[u8],
        fallback_bytes: &[u8],
        logical: (f32, f32),
    ) -> Result<Self, String> {
        let font = Font::from_bytes(font_bytes, FontSettings::default()).map_err(|e| e.to_string())?;
        let fallback = Font::from_bytes(fallback_bytes, FontSettings::default()).map_err(|e| e.to_string())?;
        unsafe {
            let prog = link(&gl, VERT, FRAG)?;
            let vbo = gl.create_buffer()?;
            let tex = gl.create_texture()?;
            let atlas = Atlas::new(1024);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::ALPHA as i32,
                atlas.size as i32,
                atlas.size as i32,
                0,
                glow::ALPHA,
                glow::UNSIGNED_BYTE,
                Some(&atlas.pixels),
            );
            let u_view = gl.get_uniform_location(prog, "u_view");
            let u_aa = gl.get_uniform_location(prog, "u_aa");
            let mut r = Renderer {
                gl,
                prog,
                vbo,
                tex,
                u_view,
                u_aa,
                atlas,
                font,
                fallback,
                adv_cache: RefCell::new(HashMap::new()),
                verts: Vec::with_capacity(FLOATS * 6 * 4096),
                logical,
                phys: (logical.0 as u32, logical.1 as u32),
                scale: 1.0,
                vp: (0, 0, logical.0 as i32, logical.1 as i32),
                clip: None,
                draw_calls: 0,
                images: HashMap::new(),
                pending: HashSet::new(),
                failed: HashSet::new(),
                frame: 0,
            };
            r.atlas.dirty = false;
            r.set_drawable_size(r.phys.0, r.phys.1);
            Ok(r)
        }
    }

    /// Call at start and whenever the window's drawable size changes. The UI
    /// is letterboxed to keep the design aspect ratio.
    pub fn set_drawable_size(&mut self, pw: u32, ph: u32) {
        self.phys = (pw, ph);
        self.scale = (pw as f32 / self.logical.0).min(ph as f32 / self.logical.1);
        let (w, h) = ((self.logical.0 * self.scale) as i32, (self.logical.1 * self.scale) as i32);
        self.vp = ((pw as i32 - w) / 2, (ph as i32 - h) / 2, w, h);
        self.adv_cache.borrow_mut().clear();
        self.resize_atlas(if self.scale >= 1.5 { ATLAS_PX_4K } else { ATLAS_PX });
    }

    /// Replace the glyph atlas (and its GL texture) when the scale changed enough to need a
    /// different one. Glyphs are re-rasterised lazily; nothing else has to know.
    fn resize_atlas(&mut self, size: u32) {
        if self.atlas.size == size {
            return;
        }
        let Ok(tex) = (unsafe { self.gl.create_texture() }) else { return };
        let atlas = Atlas::new(size);
        unsafe {
            let gl = &self.gl;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::ALPHA as i32,
                size as i32,
                size as i32,
                0,
                glow::ALPHA,
                glow::UNSIGNED_BYTE,
                Some(&atlas.pixels),
            );
        }
        let old = std::mem::replace(&mut self.tex, tex);
        unsafe { self.gl.delete_texture(old) };
        self.atlas = atlas;
        self.atlas.dirty = false;
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Edge of the glyph atlas currently in use (1024, or 2048 above 1.5× scale).
    pub fn atlas_size(&self) -> u32 {
        self.atlas.size
    }

    fn px(&self, size: f32) -> u16 {
        (size * self.scale).round().clamp(1.0, 512.0) as u16
    }

    /// Which font supplies `ch`: 0 = UI font, 1 = fallback. Coverage is a property of the font
    /// bytes, so this is stable and cheap enough to ask per glyph.
    fn glyph_font(&self, ch: char) -> u8 {
        if self.font.lookup_glyph_index(ch) != 0 {
            0
        } else {
            1
        }
    }

    fn advance(&self, ch: char, px: u16) -> f32 {
        if let Some(adv) = self.adv_cache.borrow().get(&(ch, px)) {
            return *adv;
        }
        let font = if self.glyph_font(ch) == 0 { &self.font } else { &self.fallback };
        let adv = font.metrics(ch, px as f32).advance_width;
        self.adv_cache.borrow_mut().insert((ch, px), adv);
        adv
    }

    pub fn render(&mut self, list: &DrawList, clear: Color) {
        self.frame += 1;
        if self.atlas.full {
            self.atlas.reset(); // glyphs re-rasterise lazily over the next frame
        }
        self.verts.clear();
        self.clip = None;
        self.draw_calls = 0;
        unsafe {
            let gl = &self.gl;
            gl.disable(glow::SCISSOR_TEST);
            gl.viewport(0, 0, self.phys.0 as i32, self.phys.1 as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.viewport(self.vp.0, self.vp.1, self.vp.2, self.vp.3);
            // Fill the letterboxed area with the theme background.
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(self.vp.0, self.vp.1, self.vp.2, self.vp.3);
            gl.clear_color(clear.r, clear.g, clear.b, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            gl.use_program(Some(self.prog));
            gl.uniform_2_f32(self.u_view.as_ref(), self.logical.0, self.logical.1);
            gl.uniform_1_f32(self.u_aa.as_ref(), 1.0 / self.scale);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
        }

        for cmd in &list.cmds {
            match cmd {
                Cmd::Rect { rect, color, radius } => self.push_rect(*rect, *color, *radius),
                Cmd::Text { x, y, size, color, text } => self.push_text(*x, *y, *size, *color, text),
                Cmd::Image { rect, key, radius, alpha } => self.draw_image(*rect, *key, *radius, *alpha),
                Cmd::Clip(c) => {
                    self.flush();
                    self.clip = *c;
                    self.apply_scissor();
                }
            }
        }
        self.flush();
    }

    fn apply_scissor(&self) {
        let (vx, vy, vw, vh) = self.vp;
        let (x, y, w, h) = match self.clip {
            Some(c) => {
                let s = self.scale;
                let x0 = (c.x * s).floor().max(0.0) as i32;
                let x1 = ((c.right() * s).ceil() as i32).min(vw);
                let y0 = (c.y * s).floor().max(0.0) as i32;
                let y1 = ((c.bottom() * s).ceil() as i32).min(vh);
                (vx + x0, vy + (vh - y1), (x1 - x0).max(0), (y1 - y0).max(0))
            }
            None => (vx, vy, vw, vh),
        };
        unsafe {
            self.gl.scissor(x, y, w, h);
        }
    }

    fn flush(&mut self) {
        if self.verts.is_empty() {
            return;
        }
        unsafe {
            let gl = &self.gl;
            if self.atlas.dirty {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    self.atlas.size as i32,
                    self.atlas.size as i32,
                    glow::ALPHA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(&self.atlas.pixels),
                );
                self.atlas.dirty = false;
            }
            let bytes = std::slice::from_raw_parts(self.verts.as_ptr() as *const u8, self.verts.len() * 4);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            // (location, components, byte offset) — must match bind_attrib_location below.
            for (loc, n, off) in [(0u32, 2, 0), (1, 2, 8), (2, 2, 16), (3, 3, 24), (4, 1, 36), (5, 4, 40)] {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(loc, n, glow::FLOAT, false, STRIDE, off);
            }
            gl.draw_arrays(glow::TRIANGLES, 0, (self.verts.len() / FLOATS) as i32);
        }
        self.draw_calls += 1;
        self.verts.clear();
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(&mut self, p: [f32; 4], uv: [f32; 4], local: [f32; 4], rect: [f32; 3], mode: f32, c: Color) {
        let corners = [(0, 0), (1, 0), (1, 1), (0, 0), (1, 1), (0, 1)];
        for (cx, cy) in corners {
            let pick = |a: f32, b: f32, i: i32| if i == 0 { a } else { b };
            self.verts.extend_from_slice(&[
                pick(p[0], p[2], cx),
                pick(p[1], p[3], cy),
                pick(uv[0], uv[2], cx),
                pick(uv[1], uv[3], cy),
                pick(local[0], local[2], cx),
                pick(local[1], local[3], cy),
                rect[0],
                rect[1],
                rect[2],
                mode,
                c.r,
                c.g,
                c.b,
                c.a,
            ]);
        }
    }

    /// Draw a cover texture. Each image is its own draw call (the atlas texture is rebound after).
    fn draw_image(&mut self, r: Rect, key: u64, radius: f32, alpha: f32) {
        let tex = match self.images.get_mut(&key) {
            Some(img) => {
                img.last_used = self.frame;
                img.tex
            }
            None => return,
        };
        self.flush();
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        }
        let (hw, hh) = (r.w / 2.0, r.h / 2.0);
        let rad = radius.clamp(0.0, hw.min(hh));
        self.quad(
            [r.x, r.y, r.right(), r.bottom()],
            [0.0, 0.0, 1.0, 1.0],
            [-hw, -hh, hw, hh],
            [hw, hh, rad],
            2.0,
            Color::rgba(1.0, 1.0, 1.0, alpha),
        );
        self.flush();
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
        }
    }

    /// True if this key was not already requested/loaded/failed (so the caller should start a load).
    pub fn mark_pending(&mut self, key: u64) -> bool {
        if self.images.contains_key(&key) || self.failed.contains(&key) {
            return false;
        }
        self.pending.insert(key)
    }

    pub fn mark_failed(&mut self, key: u64) {
        self.pending.remove(&key);
        self.failed.insert(key);
    }

    /// Upload a decoded RGB8 image as a texture. Must run on the GL thread.
    pub fn upload_image(&mut self, key: u64, rgb: &[u8], w: u32, h: u32) {
        self.upload(key, rgb, w, h, 3);
    }

    /// Same, for RGBA8 (the brand logos: white pixels, shaped by their alpha).
    pub fn upload_rgba_image(&mut self, key: u64, rgba: &[u8], w: u32, h: u32) {
        self.upload(key, rgba, w, h, 4);
    }

    /// `channels` is 3 (RGB) or 4 (RGBA).
    fn upload(&mut self, key: u64, px: &[u8], w: u32, h: u32, channels: usize) {
        self.pending.remove(&key);
        if w == 0 || h == 0 || px.len() < (w as usize) * (h as usize) * channels {
            self.failed.insert(key);
            return;
        }
        let format = if channels == 4 { glow::RGBA } else { glow::RGB };
        let budget = if self.scale >= 1.5 { MAX_IMAGES_4K } else { MAX_IMAGES };
        if self.images.len() >= budget {
            let oldest = self.images.iter().min_by_key(|(_, i)| i.last_used).map(|(k, _)| *k);
            if let Some(old) = oldest {
                if let Some(img) = self.images.remove(&old) {
                    unsafe {
                        self.gl.delete_texture(img.tex);
                    }
                }
            }
        }
        let tex = match unsafe { self.gl.create_texture() } {
            Ok(t) => t,
            Err(_) => {
                self.failed.insert(key);
                return;
            }
        };
        unsafe {
            let gl = &self.gl;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                format as i32,
                w as i32,
                h as i32,
                0,
                format,
                glow::UNSIGNED_BYTE,
                Some(px),
            );
            // Restore the atlas binding for text.
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
        }
        self.images.insert(key, GpuImage { tex, last_used: self.frame, w, h });
    }

    fn push_rect(&mut self, r: Rect, c: Color, radius: f32) {
        let (hw, hh) = (r.w / 2.0, r.h / 2.0);
        let rad = radius.clamp(0.0, hw.min(hh));
        let e = 1.0; // quad grows by 1px so AA at the edge isn't clipped
        self.quad(
            [r.x - e, r.y - e, r.right() + e, r.bottom() + e],
            [0.0; 4],
            [-hw - e, -hh - e, hw + e, hh + e],
            [hw, hh, rad],
            0.0,
            c,
        );
    }

    fn push_text(&mut self, x: f32, y: f32, size: f32, c: Color, text: &str) {
        let px = self.px(size);
        let s = self.scale;
        let ascent = self.font.horizontal_line_metrics(px as f32).map_or(px as f32 * 0.9, |m| m.ascent);
        let baseline = (y * s + ascent).round();
        let mut pen = x * s;
        for ch in text.chars() {
            let adv = self.advance(ch, px);
            let slot = self.glyph_font(ch);
            let glyph = if slot == 0 {
                self.atlas.glyph(&self.font, 0, ch, px)
            } else {
                self.atlas.glyph(&self.fallback, 1, ch, px)
            };
            if let Some(g) = glyph {
                if g.w > 0.0 {
                    let gx = (pen + g.xmin).round();
                    let gy = baseline - (g.h + g.ymin);
                    self.quad(
                        [gx / s, gy / s, (gx + g.w) / s, (gy + g.h) / s],
                        g.uv,
                        [0.0; 4],
                        [0.0; 3],
                        1.0,
                        c,
                    );
                }
            }
            pen += adv;
        }
    }

    /// RGBA8 pixels of the full drawable, top row first (for screenshots).
    pub fn read_rgba(&self) -> (Vec<u8>, u32, u32) {
        let (w, h) = self.phys;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut buf),
            );
        }
        let row = (w * 4) as usize;
        let mut flipped = Vec::with_capacity(buf.len());
        for y in (0..h as usize).rev() {
            flipped.extend_from_slice(&buf[y * row..(y + 1) * row]);
        }
        (flipped, w, h)
    }
}

impl Images for Renderer {
    fn density(&self) -> f32 {
        self.scale
    }

    fn size(&self, key: u64) -> Option<(u32, u32)> {
        self.images.get(&key).map(|i| (i.w, i.h))
    }

    fn state(&self, key: u64) -> ImgState {
        if self.images.contains_key(&key) {
            ImgState::Ready
        } else if self.failed.contains(&key) {
            ImgState::Failed
        } else if self.pending.contains(&key) {
            ImgState::Pending
        } else {
            ImgState::Missing
        }
    }
}

impl TextMetrics for Renderer {
    fn text_width(&self, text: &str, size: f32) -> f32 {
        let px = self.px(size);
        text.chars().map(|c| self.advance(c, px)).sum::<f32>() / self.scale
    }
}

unsafe fn link(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    let prog = gl.create_program()?;
    let mut shaders = Vec::new();
    for (ty, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
        let sh = gl.create_shader(ty)?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            return Err(format!("shader compile error: {}", gl.get_shader_info_log(sh)));
        }
        gl.attach_shader(prog, sh);
        shaders.push(sh);
    }
    // Locations must match the table in `flush`.
    for (i, name) in ["a_pos", "a_uv", "a_local", "a_rect", "a_mode", "a_color"].iter().enumerate() {
        gl.bind_attrib_location(prog, i as u32, name);
    }
    gl.link_program(prog);
    if !gl.get_program_link_status(prog) {
        return Err(format!("program link error: {}", gl.get_program_info_log(prog)));
    }
    for sh in shaders {
        gl.detach_shader(prog, sh);
        gl.delete_shader(sh);
    }
    Ok(prog)
}

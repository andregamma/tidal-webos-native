use fontdue::Font;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    /// Atlas UVs: u0, v0, u1, v1.
    pub uv: [f32; 4],
    /// Bitmap size in physical px (0 for blank glyphs like space).
    pub w: f32,
    pub h: f32,
    pub xmin: f32,
    pub ymin: f32,
}

/// Shelf-packed A8 glyph atlas, filled lazily.
pub struct Atlas {
    pub size: u32,
    pub pixels: Vec<u8>,
    pub dirty: bool,
    /// Set when it ran out of space; the renderer resets between frames.
    pub full: bool,
    /// Key: (char, physical px, font slot 0 = UI font / 1 = symbol fallback).
    glyphs: HashMap<(char, u16, u8), Glyph>,
    x: u32,
    y: u32,
    row_h: u32,
}

impl Atlas {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            pixels: vec![0; (size * size) as usize],
            dirty: true,
            full: false,
            glyphs: HashMap::new(),
            x: 0,
            y: 0,
            row_h: 0,
        }
    }

    pub fn reset(&mut self) {
        self.pixels.iter_mut().for_each(|p| *p = 0);
        self.glyphs.clear();
        self.x = 0;
        self.y = 0;
        self.row_h = 0;
        self.full = false;
        self.dirty = true;
    }

    /// Rasterise on first use. Returns `None` only if the atlas is full.
    /// `slot` distinguishes the two fonts that can supply a char (see `Renderer::glyph_font`).
    pub fn glyph(&mut self, font: &Font, slot: u8, ch: char, px: u16) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&(ch, px, slot)) {
            return Some(*g);
        }
        let (m, bitmap) = font.rasterize(ch, px as f32);
        let (w, h) = (m.width as u32, m.height as u32);
        let mut g = Glyph { uv: [0.0; 4], w: w as f32, h: h as f32, xmin: m.xmin as f32, ymin: m.ymin as f32 };
        if w > 0 && h > 0 {
            // 1px padding on every side avoids bleeding with linear filtering.
            let (cw, ch_) = (w + 2, h + 2);
            if self.x + cw > self.size {
                self.x = 0;
                self.y += self.row_h;
                self.row_h = 0;
            }
            if self.y + ch_ > self.size {
                self.full = true;
                return None;
            }
            for row in 0..h {
                let dst = ((self.y + 1 + row) * self.size + self.x + 1) as usize;
                let src = (row * w) as usize;
                self.pixels[dst..dst + w as usize].copy_from_slice(&bitmap[src..src + w as usize]);
            }
            let s = self.size as f32;
            g.uv = [
                (self.x + 1) as f32 / s,
                (self.y + 1) as f32 / s,
                (self.x + 1 + w) as f32 / s,
                (self.y + 1 + h) as f32 / s,
            ];
            self.x += cw;
            self.row_h = self.row_h.max(ch_);
            self.dirty = true;
        }
        self.glyphs.insert((ch, px, slot), g);
        Some(g)
    }
}

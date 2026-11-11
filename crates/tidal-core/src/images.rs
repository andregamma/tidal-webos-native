//! The view's window onto loaded cover images. The renderer owns the GPU textures and
//! implements [`Images`]; screens only ask for state and emit `DrawList::image`/`want`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImgState {
    /// Texture is on the GPU: draw it.
    Ready,
    /// A download/decode is in flight.
    Pending,
    /// It failed; do not ask again (the placeholder stays).
    Failed,
    /// Never requested: the caller should `want` it.
    Missing,
}

pub trait Images {
    fn state(&self, key: u64) -> ImgState;

    /// Pixel size of a loaded image, when the platform knows it. Lets views keep an aspect
    /// ratio (the brand logos) instead of stretching into the box they are given.
    fn size(&self, _key: u64) -> Option<(u32, u32)> {
        None
    }

    /// Physical pixels per logical pixel of the drawable: 1.0 on a 1080p window, 2.0 on a 4K
    /// one. Views use it to ask for assets large enough for what they actually draw.
    fn density(&self) -> f32 {
        1.0
    }
}

/// For headless use and tests: every image counts as failed, so nothing is requested.
pub struct NoImages;

impl Images for NoImages {
    fn state(&self, _key: u64) -> ImgState {
        ImgState::Failed
    }
}

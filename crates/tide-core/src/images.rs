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
}

/// For headless use and tests: every image counts as failed, so nothing is requested.
pub struct NoImages;

impl Images for NoImages {
    fn state(&self, _key: u64) -> ImgState {
        ImgState::Failed
    }
}

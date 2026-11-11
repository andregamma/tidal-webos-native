/// Frame-rate independent exponential smoothing toward a target.
///
/// Used for everything that moves: scroll offsets, focus highlights, sidebar
/// expansion, fades. `value` is what you draw; set `target` when state changes.
#[derive(Clone, Copy, Debug)]
pub struct Smooth {
    pub value: f32,
    pub target: f32,
    rate: f32,
}

impl Smooth {
    pub fn new(v: f32) -> Self {
        Self { value: v, target: v, rate: 14.0 }
    }
    pub fn with_rate(mut self, rate: f32) -> Self {
        self.rate = rate;
        self
    }
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }
    pub fn snap(&mut self, v: f32) {
        self.value = v;
        self.target = v;
    }
    pub fn update(&mut self, dt: f32) {
        let k = 1.0 - (-self.rate * dt).exp();
        self.value += (self.target - self.value) * k;
        if (self.target - self.value).abs() < 0.005 {
            self.value = self.target;
        }
    }
    pub fn settled(&self) -> bool {
        self.value == self.target
    }
}

impl Default for Smooth {
    fn default() -> Self {
        Smooth::new(0.0)
    }
}

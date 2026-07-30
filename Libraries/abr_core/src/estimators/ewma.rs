#[derive(Debug, Clone)]
pub struct Ewma {
    value: f64,
    initialized: bool,
    alpha: f64,
}

impl Ewma {
    pub fn new(alpha: f64) -> Self {
        Self {
            value: 0.0,
            initialized: false,
            alpha,
        }
    }

    pub fn new_initialized(alpha: f64, value: f64) -> Self {
        Self {
            value,
            initialized: true,
            alpha,
        }
    }

    pub fn set_alpha(&mut self, alpha: f64) {
        self.alpha = alpha;
    }

    pub fn reset(&mut self) {
        self.value = 0.0;
        self.initialized = false;
    }

    pub fn update(&mut self, sample: f64) -> Option<f64> {
        if !sample.is_finite() {
            return None;
        }

        self.value = if self.initialized {
            self.alpha * sample + (1.0 - self.alpha) * self.value
        } else {
            self.initialized = true;
            sample
        };

        Some(self.value)
    }

    pub fn value(&self) -> Option<f64> {
        self.initialized.then_some(self.value)
    }

    pub fn value_or(&self, fallback: f64) -> f64 {
        self.value().unwrap_or(fallback)
    }
}

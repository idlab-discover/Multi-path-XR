#[derive(Default, Clone)]
pub struct Series {
    pub vals: Vec<f64>,
}
impl Series {
    pub fn push_ms(&mut self, d: std::time::Duration) {
        self.vals.push(d.as_secs_f64() * 1000.0);
    }
    pub fn push(&mut self, v: f64) {
        self.vals.push(v);
    }
    pub fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }
    pub fn mean(&self) -> f64 {
        if self.vals.is_empty() {
            0.0
        } else {
            self.vals.iter().sum::<f64>() / self.vals.len() as f64
        }
    }
    pub fn stddev(&self) -> f64 {
        if self.vals.len() < 2 {
            return 0.0;
        }
        let m = self.mean();
        let var = self.vals.iter().map(|v| (v - m) * (v - m)).sum::<f64>()
            / (self.vals.len() as f64 - 1.0);
        var.sqrt()
    }
    pub fn pct(&mut self, p: f64) -> f64 {
        if self.vals.is_empty() {
            return 0.0;
        }
        self.vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((p / 100.0) * (self.vals.len() as f64 - 1.0)).round() as usize;
        self.vals[idx]
    }
}

#[derive(Default)]
pub struct TimeStats {
    pub enc_ms: Series,
    pub chunk_ms: Series,
    pub reasm_ms: Series,
    pub dec_ms: Series,
    pub e2e_ms: Series,
}

#[derive(Default)]
pub struct SizeStats {
    pub frame_bytes: Series,  // whole PCF frame
    pub header_bytes: Series, // PCF + chunk headers sum
    pub chunks: Series,
    pub i_sizes: Series,
    pub p_sizes: Series,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualityId(u32);

impl QualityId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn from_index(index: usize) -> Self {
        Self(index as u32)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }

    pub const fn as_index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityDescriptor {
    pub id: QualityId,
    pub nominal_bitrate_bps: u64,
    pub utility: f64,
    pub enabled: bool,
}

impl QualityDescriptor {
    pub const fn new(id: QualityId, nominal_bitrate_bps: u64) -> Self {
        Self {
            id,
            nominal_bitrate_bps,
            utility: 0.0,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QualityLadder {
    qualities: Vec<QualityDescriptor>,
}

impl QualityLadder {
    pub fn new(qualities: Vec<QualityDescriptor>) -> Self {
        Self { qualities }
    }

    pub fn as_slice(&self) -> &[QualityDescriptor] {
        &self.qualities
    }
}

use crate::model::{QualityDescriptor, QualityId};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardConstraints {
    pub min_quality: Option<QualityId>,
    pub max_quality: Option<QualityId>,
    pub max_bitrate_bps: Option<u64>,
    pub disabled_qualities: Vec<QualityId>,
}

impl HardConstraints {
    pub fn allows(&self, quality: &QualityDescriptor) -> bool {
        if self.disabled_qualities.contains(&quality.id) {
            return false;
        }

        if let Some(min_quality) = self.min_quality {
            if quality.id.as_u32() < min_quality.as_u32() {
                return false;
            }
        }

        if let Some(max_quality) = self.max_quality {
            if quality.id.as_u32() > max_quality.as_u32() {
                return false;
            }
        }

        if let Some(max_bitrate_bps) = self.max_bitrate_bps {
            if quality.nominal_bitrate_bps > max_bitrate_bps {
                return false;
            }
        }

        true
    }
}

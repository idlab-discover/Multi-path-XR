use crate::config::{AbrConfig, AbrMode};
use crate::engine::Abr;
use crate::model::QualityLadder;

#[derive(Debug, Clone)]
pub struct AbrFactory {
    config: AbrConfig,
    quality_ladder: QualityLadder,
}

impl AbrFactory {
    pub fn new_default(mode: AbrMode) -> Self {
        Self {
            config: AbrConfig::for_mode(mode),
            quality_ladder: QualityLadder::default(),
        }
    }

    pub fn with_config(mut self, config: AbrConfig) -> Self {
        self.config = config;
        self
    }

    pub fn with_quality_ladder(mut self, quality_ladder: QualityLadder) -> Self {
        self.quality_ladder = quality_ladder;
        self
    }

    pub fn build(self) -> Abr {
        Abr::new(self.config, self.quality_ladder)
    }
}

use crate::diagnostics::DecisionDiagnostics;
use crate::model::quality::QualityId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankedQuality {
    pub id: QualityId,
    pub nominal_bitrate_bps: u64,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub allowed_qualities: Vec<RankedQuality>,
    pub recommended_quality: Option<RankedQuality>,
    pub estimated_bandwidth_bps: f64,
    pub diagnostics: DecisionDiagnostics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AbrSelectionDecision {
    pub allowed_qualities: Vec<RankedQuality>,
    pub recommended_quality: Option<RankedQuality>,
    pub estimated_bandwidth_bps: f64,
    pub selected_quality_ids: Vec<QualityId>,
    pub selected_total_bitrate_bps: Option<u64>,
    pub diagnostics: DecisionDiagnostics,
}

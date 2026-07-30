use crate::config::AbrMode;

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionDiagnostics {
    pub mode: AbrMode,
    pub estimated_bandwidth_bps: f64,
    pub bandwidth_budget_bps: f64,
    pub risk_adjusted_bandwidth_budget_bps: f64,
    pub latency_risk_fraction: f64,
    pub filtered_by_constraints: usize,
    pub filtered_by_bandwidth: usize,
    pub fallback_to_lowest: bool,
    pub held_by_upswitch_hysteresis: bool,
    pub held_by_downswitch_hysteresis: bool,
    pub blocked_by_hold_down: bool,
    pub blocked_by_buffer_guard: bool,
}

impl DecisionDiagnostics {
    pub fn empty(mode: AbrMode, estimated_bandwidth_bps: f64, bandwidth_budget_bps: f64) -> Self {
        Self {
            mode,
            estimated_bandwidth_bps,
            bandwidth_budget_bps,
            risk_adjusted_bandwidth_budget_bps: bandwidth_budget_bps,
            latency_risk_fraction: 0.0,
            filtered_by_constraints: 0,
            filtered_by_bandwidth: 0,
            fallback_to_lowest: false,
            held_by_upswitch_hysteresis: false,
            held_by_downswitch_hysteresis: false,
            blocked_by_hold_down: false,
            blocked_by_buffer_guard: false,
        }
    }
}

pub(crate) mod abr;
pub mod mpd;
pub mod player;
pub mod segment;
use bytes::Bytes;

#[derive(Clone, Debug, Default)]
pub struct DashNetworkStats {
    pub ttfb_ms: f64,
    pub server_wait_ms: Option<u64>,
    pub estimated_one_way_latency_ms: Option<f64>,
    pub estimated_rtt_ms: Option<f64>,
    pub serving_hop_clock_offset_ms: Option<f64>,
    pub origin_clock_offset_ms: Option<f64>,
    pub origin_clock_source_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DashAbrStats {
    pub estimated_bandwidth_bps: u64,
    pub bandwidth_budget_bps: u64,
    pub risk_adjusted_bandwidth_budget_bps: u64,
    pub last_throughput_sample_bps: u64,
    pub last_whole_request_throughput_sample_bps: u64,
    pub requested_representation_bitrate_bps: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DashPlayerTimingStats {
    pub current_latency_ms: f64,
    pub fetch_loop_lateness_ms: f64,
    pub segment_number_vs_clock_delta: i64,
}

/// Events emitted by the player
pub enum DashEvent {
    Segment {
        data: Bytes,
        content_type: String,
        representation_id: String,
        segment_number: u64,
        duration: f64,
        url: String,
        playback_rate: f64,
        network_stats: DashNetworkStats,
        abr_stats: DashAbrStats,
        timing_stats: DashPlayerTimingStats,
    },
    EmptySegment {
        segment_number: u64,
    },
    DownloadError {
        url: String,
        reason: String,
    },
    Info(String),
    Warning(String),
}

pub use player::DashPlayer;

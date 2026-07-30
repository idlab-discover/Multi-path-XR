use crate::mpd::Representation;
use crate::segment::fetcher::SegmentDownload;
use crate::DashNetworkStats;
use abr_core::{AbrConfig, AbrController, AbrMode, Observation, QualityId, QualitySelectionPolicy};
use std::collections::BTreeMap;

const MIN_TRUSTWORTHY_BODY_BYTES: usize = 32 * 1024;
const MIN_TRUSTWORTHY_BODY_DURATION_S: f64 = 0.010;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThroughputSampleSource {
    Body,
    TotalFallback,
}

pub(crate) fn build_dash_abr(reps: &[Representation], mode: AbrMode) -> AbrController {
    let mut abr = AbrController::new(
        AbrConfig::for_mode(mode),
        QualitySelectionPolicy::subscriber_with_composite_qualities(false),
    );
    abr.update_quality_ladder(&build_quality_bitrates(reps));
    abr
}

pub(crate) fn build_quality_bitrates(reps: &[Representation]) -> BTreeMap<QualityId, u64> {
    reps.iter()
        .enumerate()
        .map(|(index, rep)| (QualityId::from_index(index), rep.bandwidth))
        .collect()
}

pub(crate) fn observation_from_segment_download(
    segment_download: &SegmentDownload,
    network_stats: &DashNetworkStats,
    segment_duration_s: f64,
    playback_headroom_s: f64,
    decision_time_ms: u64,
) -> Observation {
    let (throughput_sample_bps, _) = throughput_sample_from_segment_download(segment_download);
    let has_latency_reference = segment_duration_s > 0.0;

    Observation {
        throughput_sample_bps: Some(throughput_sample_bps),
        playback_buffer_s: has_latency_reference.then_some(playback_headroom_s.max(0.0)),
        decision_time_ms: Some(decision_time_ms),
        time_to_first_byte_s: has_latency_reference
            .then_some(ttfb_without_server_wait_s(segment_download)),
        estimated_rtt_s: has_latency_reference
            .then(|| network_stats.estimated_rtt_ms.map(|ms| ms / 1000.0))
            .flatten(),
        completion_time_s: has_latency_reference
            .then_some(completion_time_without_server_wait_s(segment_download)),
        segment_duration_s: has_latency_reference.then_some(segment_duration_s),
        pacing_rate_bps: None,
        congestion_window_bytes: None,
        lost_packets_delta: None,
        lost_bytes_delta: None,
    }
}

pub(crate) fn whole_request_throughput_sample_from_segment_download(
    segment_download: &SegmentDownload,
) -> f64 {
    ((segment_download.bytes.len() as f64) * 8.0)
        / completion_time_without_server_wait_s(segment_download).max(f64::EPSILON)
}

fn throughput_sample_from_segment_download(
    segment_download: &SegmentDownload,
) -> (f64, ThroughputSampleSource) {
    let trustworthy_body = segment_download.body_s >= MIN_TRUSTWORTHY_BODY_DURATION_S
        && segment_download.bytes.len() >= MIN_TRUSTWORTHY_BODY_BYTES;
    let duration_s = if trustworthy_body {
        segment_download.body_s
    } else {
        completion_time_without_server_wait_s(segment_download).max(f64::EPSILON)
    };
    let source = if trustworthy_body {
        ThroughputSampleSource::Body
    } else {
        ThroughputSampleSource::TotalFallback
    };

    (
        ((segment_download.bytes.len() as f64) * 8.0) / duration_s,
        source,
    )
}

fn ttfb_without_server_wait_s(segment_download: &SegmentDownload) -> f64 {
    match segment_download.server_wait_ms {
        Some(server_wait_ms) => {
            (segment_download.ttfb_s - (server_wait_ms as f64 / 1000.0)).max(0.0)
        }
        None => segment_download.ttfb_s,
    }
}

fn completion_time_without_server_wait_s(segment_download: &SegmentDownload) -> f64 {
    (segment_download.body_s + ttfb_without_server_wait_s(segment_download))
        .max(segment_download.body_s)
}

#[cfg(test)]
mod tests {
    use super::{
        completion_time_without_server_wait_s, observation_from_segment_download,
        throughput_sample_from_segment_download, ttfb_without_server_wait_s,
        whole_request_throughput_sample_from_segment_download, ThroughputSampleSource,
    };
    use crate::segment::fetcher::{CacheStatus, SegmentDownload};
    use crate::DashNetworkStats;
    use bytes::Bytes;

    fn download(bytes_len: usize, body_s: f64, total_s: f64) -> SegmentDownload {
        SegmentDownload {
            bytes: Bytes::from(vec![0_u8; bytes_len]),
            ttfb_s: total_s - body_s,
            body_s,
            total_s,
            server_wait_ms: None,
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        }
    }

    #[test]
    fn throughput_sample_uses_body_time_for_trustworthy_samples() {
        let segment_download = download(64 * 1024, 0.020, 0.080);

        let (sample_bps, source) = throughput_sample_from_segment_download(&segment_download);

        assert_eq!(source, ThroughputSampleSource::Body);
        assert_eq!(sample_bps, (64.0 * 1024.0 * 8.0) / 0.020);
    }

    #[test]
    fn throughput_sample_falls_back_to_total_time_for_tiny_samples() {
        let segment_download = download(8 * 1024, 0.002, 0.080);

        let (sample_bps, source) = throughput_sample_from_segment_download(&segment_download);

        assert_eq!(source, ThroughputSampleSource::TotalFallback);
        assert_eq!(sample_bps, (8.0 * 1024.0 * 8.0) / 0.080);
    }

    #[test]
    fn whole_request_throughput_sample_uses_total_request_time() {
        let segment_download = download(64 * 1024, 0.020, 0.080);

        let sample_bps = whole_request_throughput_sample_from_segment_download(&segment_download);

        assert_eq!(sample_bps, (64.0 * 1024.0 * 8.0) / 0.080);
    }

    #[test]
    fn whole_request_throughput_sample_excludes_server_wait_when_available() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from(vec![0_u8; 64 * 1024]),
            ttfb_s: 0.350,
            body_s: 0.050,
            total_s: 0.400,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        let sample_bps = whole_request_throughput_sample_from_segment_download(&segment_download);

        assert!((sample_bps - ((64.0 * 1024.0 * 8.0) / 0.150)).abs() < 1.0);
    }

    #[test]
    fn ttfb_for_abr_excludes_server_wait_when_available() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from_static(b"segment"),
            ttfb_s: 0.350,
            body_s: 0.050,
            total_s: 0.400,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        assert!((ttfb_without_server_wait_s(&segment_download) - 0.100).abs() < f64::EPSILON);
    }

    #[test]
    fn observation_uses_debiased_ttfb_for_latency_risk() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from(vec![0_u8; 64 * 1024]),
            ttfb_s: 0.350,
            body_s: 0.050,
            total_s: 0.400,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        let observation = observation_from_segment_download(
            &segment_download,
            &DashNetworkStats::default(),
            1.0,
            0.2,
            123,
        );

        let time_to_first_byte_s = observation.time_to_first_byte_s.unwrap();
        assert!((time_to_first_byte_s - 0.100).abs() < f64::EPSILON);
    }

    #[test]
    fn completion_time_for_abr_excludes_server_wait_when_available() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from_static(b"segment"),
            ttfb_s: 0.350,
            body_s: 0.050,
            total_s: 0.400,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        assert!(
            (completion_time_without_server_wait_s(&segment_download) - 0.150).abs() < f64::EPSILON
        );
    }

    #[test]
    fn tiny_sample_fallback_excludes_server_wait_from_throughput_sample() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from(vec![0_u8; 8 * 1024]),
            ttfb_s: 0.350,
            body_s: 0.002,
            total_s: 0.352,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        let (sample_bps, source) = throughput_sample_from_segment_download(&segment_download);

        assert_eq!(source, ThroughputSampleSource::TotalFallback);
        assert!((sample_bps - ((8.0 * 1024.0 * 8.0) / 0.102)).abs() < 1.0);
    }

    #[test]
    fn observation_uses_debiased_completion_time_for_latency_risk() {
        let segment_download = SegmentDownload {
            bytes: Bytes::from(vec![0_u8; 64 * 1024]),
            ttfb_s: 0.350,
            body_s: 0.050,
            total_s: 0.400,
            server_wait_ms: Some(250),
            serving_hop_now_ms: None,
            header_arrival_client_ms: None,
            cache_status: CacheStatus::Miss,
        };

        let observation = observation_from_segment_download(
            &segment_download,
            &DashNetworkStats::default(),
            1.0,
            0.2,
            123,
        );

        let completion_time_s = observation.completion_time_s.unwrap();
        assert!((completion_time_s - 0.150).abs() < f64::EPSILON);
    }
}

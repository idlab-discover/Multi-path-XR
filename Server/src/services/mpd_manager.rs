// Server/src/egress/mpd_manager.rs

use abr_core::Ewma;
use chrono::Utc;
use dash_player::mpd::builder::{MpdBuilder, RepresentationDef};
use dashmap::DashMap;
use metrics::get_metrics;
use prometheus::IntGauge;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const BANDWIDTH_EMA_ALPHA: f64 = 0.2;
const DASH_TRACK_BITRATE_BPS_HELP: &str =
    "Estimated server-side content bitrate per DASH track/representation in bits per second";
const DASH_TRACK_NETWORK_PAYLOAD_BITRATE_BPS_HELP: &str =
    "Estimated server-side wrapped DASH media-segment payload bitrate per track/representation in bits per second";

fn bandwidth_from_payload_len(payload_len_bytes: usize, fps: u64) -> u64 {
    u64::try_from(payload_len_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(fps)
        .saturating_mul(8)
}

fn bitrate_gauge_value(bitrate_bps: u64) -> i64 {
    bitrate_bps.min(i64::MAX as u64) as i64
}

#[derive(Debug, Clone, Copy)]
pub struct PayloadLengths {
    pub content_len_bytes: usize,
    pub network_payload_len_bytes: usize,
}

#[derive(Debug, Clone)]
struct BandwidthEma {
    average_bps: Ewma,
}

impl BandwidthEma {
    fn new(initial_bps: u64) -> Self {
        Self {
            average_bps: Ewma::new_initialized(BANDWIDTH_EMA_ALPHA, initial_bps as f64),
        }
    }

    fn update(&mut self, sample_bps: u64) {
        let _ = self.average_bps.update(sample_bps as f64);
    }

    fn as_u64(&self) -> u64 {
        self.average_bps.value_or(0.0).round().max(0.0) as u64
    }
}

#[derive(Debug, Clone)]
struct ManagedStream {
    fps: u64,
    bandwidth_ema: BandwidthEma,
    bitrate_metric: IntGauge,
    network_payload_bandwidth_ema: BandwidthEma,
    network_payload_bitrate_metric: IntGauge,
}

impl ManagedStream {
    fn new(
        fps: u64,
        initial_payload_lengths: PayloadLengths,
        bitrate_metric: IntGauge,
        network_payload_bitrate_metric: IntGauge,
    ) -> Self {
        let initial_bandwidth =
            bandwidth_from_payload_len(initial_payload_lengths.content_len_bytes, fps);
        let initial_network_payload_bandwidth =
            bandwidth_from_payload_len(initial_payload_lengths.network_payload_len_bytes, fps);
        bitrate_metric.set(bitrate_gauge_value(initial_bandwidth));
        network_payload_bitrate_metric.set(bitrate_gauge_value(initial_network_payload_bandwidth));
        Self {
            fps,
            bandwidth_ema: BandwidthEma::new(initial_bandwidth),
            bitrate_metric,
            network_payload_bandwidth_ema: BandwidthEma::new(initial_network_payload_bandwidth),
            network_payload_bitrate_metric,
        }
    }

    fn update_bandwidth(&mut self, payload_lengths: PayloadLengths) {
        let sample_bps = bandwidth_from_payload_len(payload_lengths.content_len_bytes, self.fps);
        let network_payload_sample_bps =
            bandwidth_from_payload_len(payload_lengths.network_payload_len_bytes, self.fps);
        self.bandwidth_ema.update(sample_bps);
        self.network_payload_bandwidth_ema
            .update(network_payload_sample_bps);
        self.bitrate_metric
            .set(bitrate_gauge_value(self.bandwidth_bps()));
        self.network_payload_bitrate_metric
            .set(bitrate_gauge_value(self.network_payload_bandwidth_bps()));
    }

    fn bandwidth_bps(&self) -> u64 {
        self.bandwidth_ema.as_u64()
    }

    fn network_payload_bandwidth_bps(&self) -> u64 {
        self.network_payload_bandwidth_ema.as_u64()
    }
}

#[derive(Debug, Clone)]
struct ManagedMpd {
    builder: MpdBuilder,
    streams: HashMap<String, ManagedStream>,
}

impl ManagedMpd {
    fn new(fps: u64) -> Self {
        Self {
            builder: MpdBuilder::live()
                .availability_start(Utc::now() - chrono::Duration::milliseconds(124))
                .time_shift_buffer(0.2)
                .segment_duration(1_000, fps * 1_000)
                .minimum_update_period(60.0)
                .suggested_presentation_delay(0.030),
            streams: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct MpdManager {
    builders: Arc<DashMap<String, Arc<RwLock<ManagedMpd>>>>,
    notify_new_group: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl std::fmt::Debug for MpdManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpdManager")
            .field("notify_new_group", &"<callback>")
            .field("builders", &self.builders.len())
            .finish()
    }
}

impl MpdManager {
    pub fn new() -> Self {
        Self {
            builders: Arc::new(DashMap::new()),
            notify_new_group: None,
        }
    }

    pub fn set_notify_callback(&mut self, callback: Arc<dyn Fn(String) + Send + Sync>) {
        self.notify_new_group = Some(callback);
    }

    pub fn add_stream_to_mpd(
        &self,
        group_id: &str,
        stream_id: &str,
        mime_type: &str,
        codecs: &str,
        initial_payload_lengths: PayloadLengths,
        fps: u64,
    ) {
        let managed = self
            .builders
            .entry(group_id.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(ManagedMpd::new(fps.max(1)))))
            .clone();

        let mut notify_new_representation = false;
        {
            let mut managed = managed.write().unwrap();
            if !managed.streams.contains_key(stream_id) {
                let bitrate_metric = get_metrics()
                    .get_or_create_labelled_gauge(
                        "dash_track_bitrate_bps",
                        DASH_TRACK_BITRATE_BPS_HELP,
                        &["group_id", "stream_id"],
                        &[group_id, stream_id],
                    )
                    .expect("failed to create DASH track bitrate metric");
                let network_payload_bitrate_metric = get_metrics()
                    .get_or_create_labelled_gauge(
                        "dash_track_network_payload_bitrate_bps",
                        DASH_TRACK_NETWORK_PAYLOAD_BITRATE_BPS_HELP,
                        &["group_id", "stream_id"],
                        &[group_id, stream_id],
                    )
                    .expect("failed to create DASH track network payload bitrate metric");
                let stream = ManagedStream::new(
                    fps.max(1),
                    initial_payload_lengths,
                    bitrate_metric,
                    network_payload_bitrate_metric,
                );

                managed.builder.representations.push(RepresentationDef {
                    id: stream_id.to_string(),
                    mime_type: mime_type.to_string(),
                    codecs: codecs.to_string(),
                    bandwidth: stream.bandwidth_bps(),
                    initialization: format!("{stream_id}/init.mp4"),
                    media: format!("{stream_id}/$Number%09d$.m4s"),
                    availability_time_offset: Some(-0.030),
                    availability_time_complete: Some(false),
                });

                managed.streams.insert(stream_id.to_string(), stream);
                notify_new_representation = true;
            }
        }

        if notify_new_representation {
            if let Some(callback) = &self.notify_new_group {
                (callback)(group_id.to_string());
            }
        }
    }

    pub fn update_stream_bandwidth(
        &self,
        group_id: &str,
        stream_id: &str,
        payload_lengths: PayloadLengths,
    ) {
        let Some(managed) = self.builders.get(group_id).map(|entry| entry.clone()) else {
            return;
        };

        let mut managed = managed.write().unwrap();
        if let Some(stream) = managed.streams.get_mut(stream_id) {
            stream.update_bandwidth(payload_lengths);
        }
    }

    pub fn get_mpd(&self, group_id: &str) -> Option<String> {
        let managed = self.builders.get(group_id).map(|entry| entry.clone())?;
        let managed = managed.read().unwrap();
        let mut builder = managed.builder.clone();

        for representation in &mut builder.representations {
            if let Some(stream) = managed.streams.get(&representation.id) {
                representation.bandwidth = stream.bandwidth_bps();
            }
        }

        builder.build_xml_string().ok()
    }

    pub fn get_groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = self
            .builders
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        groups.sort();
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ensure_metrics_initialized() {
        crate::test_support::ensure_metrics_initialized();
    }

    #[test]
    fn get_mpd_uses_smoothed_bandwidth_without_overwriting_representation_definition() {
        ensure_metrics_initialized();
        let manager = MpdManager::new();

        manager.add_stream_to_mpd(
            "group",
            "stream",
            "video/pc",
            "drc",
            PayloadLengths {
                content_len_bytes: 100,
                network_payload_len_bytes: 120,
            },
            30,
        );
        manager.update_stream_bandwidth(
            "group",
            "stream",
            PayloadLengths {
                content_len_bytes: 500,
                network_payload_len_bytes: 520,
            },
        );

        let managed = manager.builders.get("group").unwrap().clone();
        let managed = managed.read().unwrap();
        assert_eq!(
            managed.builder.representations[0].bandwidth,
            bandwidth_from_payload_len(100, 30)
        );
        drop(managed);

        let xml = manager.get_mpd("group").unwrap();
        let initial_bandwidth = bandwidth_from_payload_len(100, 30) as f64;
        let updated_bandwidth = bandwidth_from_payload_len(500, 30) as f64;
        let expected_bandwidth = ((1.0 - BANDWIDTH_EMA_ALPHA) * initial_bandwidth
            + (BANDWIDTH_EMA_ALPHA * updated_bandwidth))
            .round() as u64;
        assert!(xml.contains(&format!("bandwidth=\"{expected_bandwidth}\"")));
    }

    #[test]
    fn track_network_payload_metric_uses_wrapped_segment_size() {
        ensure_metrics_initialized();
        let manager = MpdManager::new();

        manager.add_stream_to_mpd(
            "group",
            "stream",
            "video/pc",
            "drc",
            PayloadLengths {
                content_len_bytes: 100,
                network_payload_len_bytes: 120,
            },
            30,
        );
        manager.update_stream_bandwidth(
            "group",
            "stream",
            PayloadLengths {
                content_len_bytes: 500,
                network_payload_len_bytes: 560,
            },
        );

        let managed = manager.builders.get("group").unwrap().clone();
        let managed = managed.read().unwrap();
        let stream = managed.streams.get("stream").unwrap();

        let initial_bandwidth = bandwidth_from_payload_len(120, 30) as f64;
        let updated_bandwidth = bandwidth_from_payload_len(560, 30) as f64;
        let expected_network_payload_bandwidth = ((1.0 - BANDWIDTH_EMA_ALPHA) * initial_bandwidth
            + (BANDWIDTH_EMA_ALPHA * updated_bandwidth))
            .round() as u64;

        assert_eq!(
            stream.network_payload_bandwidth_bps(),
            expected_network_payload_bandwidth
        );
    }

    #[test]
    fn notify_callback_only_fires_for_new_representations() {
        ensure_metrics_initialized();
        let notifications = Arc::new(AtomicUsize::new(0));
        let notifications_clone = notifications.clone();

        let mut manager = MpdManager::new();
        manager.set_notify_callback(Arc::new(move |_group_id| {
            notifications_clone.fetch_add(1, Ordering::Relaxed);
        }));

        manager.add_stream_to_mpd(
            "group",
            "stream_1",
            "video/pc",
            "drc",
            PayloadLengths {
                content_len_bytes: 100,
                network_payload_len_bytes: 120,
            },
            30,
        );
        manager.update_stream_bandwidth(
            "group",
            "stream_1",
            PayloadLengths {
                content_len_bytes: 200,
                network_payload_len_bytes: 220,
            },
        );
        manager.add_stream_to_mpd(
            "group",
            "stream_2",
            "video/pc",
            "drc",
            PayloadLengths {
                content_len_bytes: 300,
                network_payload_len_bytes: 340,
            },
            30,
        );

        assert_eq!(notifications.load(Ordering::Relaxed), 2);
    }
}

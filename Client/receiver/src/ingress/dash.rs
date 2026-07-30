use abr_core::AbrModeHandle;
use dashmap::{mapref::entry::Entry, DashMap};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use crate::{
    clock::{ClockDomain, ClockSampleTrust, ClockSourceKey},
    processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
    storage::ReceiverTransport,
};
use dash_player::{DashAbrStats, DashEvent, DashNetworkStats, DashPlayer, DashPlayerTimingStats};
use metrics::get_metrics;
use mp4_box::reader::extract_mdat_boxes;
use pcf::types::PCF_MAGIC;
use prometheus::IntGauge;
use tokio::{runtime::Runtime, task::JoinHandle};
use tracing::{debug, error, info, warn};
use url::Url;

type GroupEntry = (
    JoinHandle<()>,
    Arc<DashPlayer>,
    Arc<DashNetworkMetrics>,
    Arc<DashAbrMetrics>,
    Arc<DashPlayerTimingMetrics>,
    Arc<DashRequestedQualityMetrics>,
    Arc<DashRobustnessMetrics>,
);
type GroupMap = Arc<RwLock<HashMap<String, GroupEntry>>>;

const DASH_ESTIMATED_BANDWIDTH_BPS_HELP: &str =
    "Estimated DASH ABR bandwidth in bits per second for the active group";
const DASH_BANDWIDTH_BUDGET_BPS_HELP: &str =
    "Raw DASH ABR bandwidth budget in bits per second for the active group";
const DASH_RISK_ADJUSTED_BANDWIDTH_BUDGET_BPS_HELP: &str =
    "Risk-adjusted DASH ABR bandwidth budget in bits per second for the active group";
const DASH_LAST_THROUGHPUT_SAMPLE_BPS_HELP: &str =
    "Last DASH segment throughput sample used by ABR in bits per second for the active group";
const DASH_LAST_WHOLE_REQUEST_THROUGHPUT_SAMPLE_BPS_HELP: &str =
    "Last DASH segment throughput sample over the full HTTP request time after subtracting reported backend wait, in bits per second for the active group";
const DASH_REQUESTED_REPRESENTATION_BITRATE_BPS_HELP: &str =
    "Bitrate of the DASH representation currently requested by the player in bits per second for the active group";
const DASH_EMPTY_SEGMENTS_TOTAL_HELP: &str =
    "Total number of empty DASH media segments observed for the active group";
const DASH_DOWNLOAD_ERRORS_TOTAL_HELP: &str =
    "Total number of DASH segment download errors observed for the active group";
const DASH_SEGMENTS_TOTAL_HELP: &str =
    "Total number of DASH media segments observed per representation for the active group";

#[derive(Clone, Debug)]
struct DashNetworkMetricLabels {
    group_id: String,
    origin: String,
}

impl DashNetworkMetricLabels {
    fn new(group_id: &str, mpd_url: &str) -> Self {
        let origin = Url::parse(mpd_url)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| {
                    let port = url
                        .port()
                        .map(|port| format!(":{port}"))
                        .unwrap_or_default();
                    format!("{}://{}{}", url.scheme(), host, port)
                })
            })
            .unwrap_or_else(|| mpd_url.to_string());

        Self {
            group_id: group_id.to_string(),
            origin,
        }
    }

    fn names() -> [&'static str; 2] {
        ["group_id", "origin"]
    }

    fn values(&self) -> [&str; 2] {
        [self.group_id.as_str(), self.origin.as_str()]
    }
}

#[derive(Clone, Debug)]
struct DashNetworkMetrics {
    estimated_rtt_us: IntGauge,
    estimated_one_way_latency_us: IntGauge,
    ttfb_us: IntGauge,
    server_wait_us: IntGauge,
    serving_hop_clock_offset_us: IntGauge,
    origin_clock_offset_us: IntGauge,
    legacy_origin_clock_offset_us: IntGauge,
}

impl DashNetworkMetrics {
    fn new(labels: &DashNetworkMetricLabels) -> Self {
        let metrics = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();

        Self {
            estimated_rtt_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_network_rtt_us",
                    "Estimated DASH network RTT in microseconds, derived from HTTP TTFB minus backend wait time",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH RTT metric"),
            estimated_one_way_latency_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_network_one_way_latency_us",
                    "Estimated DASH client-to-server one-way network latency in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH one-way latency metric"),
            ttfb_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_network_ttfb_us",
                    "HTTP time-to-first-byte for DASH segment requests in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH TTFB metric"),
            server_wait_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_backend_wait_us",
                    "Server-reported backend wait time for DASH segment requests in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH backend wait metric"),
            serving_hop_clock_offset_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_serving_hop_clock_offset_us",
                    "Estimated serving-hop-minus-client wall-clock offset for DASH in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH serving-hop clock offset metric"),
            origin_clock_offset_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_origin_clock_offset_us",
                    "Estimated origin-server-minus-client wall-clock offset for DASH in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH origin clock offset metric"),
            legacy_origin_clock_offset_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_network_clock_offset_us",
                    "Estimated origin-server-minus-client wall-clock offset for DASH in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH clock offset metric"),
        }
    }

    fn record(&self, stats: &DashNetworkStats) {
        self.ttfb_us.set(ms_to_us_gauge(stats.ttfb_ms));
        self.server_wait_us.set(
            stats
                .server_wait_ms
                .map_or(0, |value| value.saturating_mul(1_000) as i64),
        );

        if let Some(estimated_rtt_ms) = stats.estimated_rtt_ms {
            self.estimated_rtt_us.set(ms_to_us_gauge(estimated_rtt_ms));
        }
        if let Some(estimated_one_way_latency_ms) = stats.estimated_one_way_latency_ms {
            self.estimated_one_way_latency_us
                .set(ms_to_us_gauge(estimated_one_way_latency_ms));
        }
        if let Some(serving_hop_clock_offset_ms) = stats.serving_hop_clock_offset_ms {
            self.serving_hop_clock_offset_us
                .set(signed_ms_to_us_gauge(serving_hop_clock_offset_ms));
        }
        if let Some(origin_clock_offset_ms) = stats.origin_clock_offset_ms {
            let gauge_value = signed_ms_to_us_gauge(origin_clock_offset_ms);
            self.origin_clock_offset_us.set(gauge_value);
            self.legacy_origin_clock_offset_us.set(gauge_value);
        }
    }

    fn reset(&self) {
        self.estimated_rtt_us.set(0);
        self.estimated_one_way_latency_us.set(0);
        self.ttfb_us.set(0);
        self.server_wait_us.set(0);
        self.serving_hop_clock_offset_us.set(0);
        self.origin_clock_offset_us.set(0);
        self.legacy_origin_clock_offset_us.set(0);
    }
}

#[derive(Clone, Debug)]
struct DashAbrMetrics {
    estimated_bandwidth_bps: IntGauge,
    bandwidth_budget_bps: IntGauge,
    risk_adjusted_bandwidth_budget_bps: IntGauge,
    last_throughput_sample_bps: IntGauge,
    last_whole_request_throughput_sample_bps: IntGauge,
    requested_representation_bitrate_bps: IntGauge,
}

impl DashAbrMetrics {
    fn new(labels: &DashNetworkMetricLabels) -> Self {
        let metrics = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();

        Self {
            estimated_bandwidth_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_estimated_bandwidth_bps",
                    DASH_ESTIMATED_BANDWIDTH_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH estimated bandwidth metric"),
            bandwidth_budget_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_bandwidth_budget_bps",
                    DASH_BANDWIDTH_BUDGET_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH bandwidth budget metric"),
            risk_adjusted_bandwidth_budget_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_risk_adjusted_bandwidth_budget_bps",
                    DASH_RISK_ADJUSTED_BANDWIDTH_BUDGET_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH risk-adjusted bandwidth budget metric"),
            last_throughput_sample_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_last_throughput_sample_bps",
                    DASH_LAST_THROUGHPUT_SAMPLE_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH last throughput sample metric"),
            last_whole_request_throughput_sample_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_last_whole_request_throughput_sample_bps",
                    DASH_LAST_WHOLE_REQUEST_THROUGHPUT_SAMPLE_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH whole-request throughput sample metric"),
            requested_representation_bitrate_bps: metrics
                .get_or_create_labelled_gauge(
                    "dash_requested_representation_bitrate_bps",
                    DASH_REQUESTED_REPRESENTATION_BITRATE_BPS_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH requested representation bitrate metric"),
        }
    }

    fn record(&self, stats: &DashAbrStats) {
        self.estimated_bandwidth_bps
            .set(u64_to_i64_gauge(stats.estimated_bandwidth_bps));
        self.bandwidth_budget_bps
            .set(u64_to_i64_gauge(stats.bandwidth_budget_bps));
        self.risk_adjusted_bandwidth_budget_bps
            .set(u64_to_i64_gauge(stats.risk_adjusted_bandwidth_budget_bps));
        self.last_throughput_sample_bps
            .set(u64_to_i64_gauge(stats.last_throughput_sample_bps));
        self.last_whole_request_throughput_sample_bps
            .set(u64_to_i64_gauge(
                stats.last_whole_request_throughput_sample_bps,
            ));
        self.requested_representation_bitrate_bps
            .set(u64_to_i64_gauge(stats.requested_representation_bitrate_bps));
    }

    fn reset(&self) {
        self.estimated_bandwidth_bps.set(0);
        self.bandwidth_budget_bps.set(0);
        self.risk_adjusted_bandwidth_budget_bps.set(0);
        self.last_throughput_sample_bps.set(0);
        self.last_whole_request_throughput_sample_bps.set(0);
        self.requested_representation_bitrate_bps.set(0);
    }
}

#[derive(Clone, Debug)]
struct DashPlayerTimingMetrics {
    current_latency_us: IntGauge,
    fetch_loop_lateness_us: IntGauge,
    segment_number_vs_clock_delta: IntGauge,
}

impl DashPlayerTimingMetrics {
    fn new(labels: &DashNetworkMetricLabels) -> Self {
        let metrics = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();

        Self {
            current_latency_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_player_current_latency_us",
                    "DASH player live-edge latency for the currently requested segment in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH player current latency metric"),
            fetch_loop_lateness_us: metrics
                .get_or_create_labelled_gauge(
                    "dash_fetch_loop_lateness_us",
                    "Positive DASH fetch-loop lateness relative to target live latency in microseconds",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH fetch loop lateness metric"),
            segment_number_vs_clock_delta: metrics
                .get_or_create_labelled_gauge(
                    "dash_segment_number_vs_clock_delta",
                    "Requested DASH segment number minus the segment number implied by wall-clock live edge and target latency",
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH segment-clock delta metric"),
        }
    }

    fn record(&self, stats: &DashPlayerTimingStats) {
        self.current_latency_us
            .set(ms_to_us_gauge(stats.current_latency_ms));
        self.fetch_loop_lateness_us
            .set(ms_to_us_gauge(stats.fetch_loop_lateness_ms));
        self.segment_number_vs_clock_delta
            .set(stats.segment_number_vs_clock_delta);
    }

    fn reset(&self) {
        self.current_latency_us.set(0);
        self.fetch_loop_lateness_us.set(0);
        self.segment_number_vs_clock_delta.set(0);
    }
}

#[derive(Debug)]
struct DashRequestedQualityMetrics {
    switches_up_total: IntGauge,
    switches_down_total: IntGauge,
    last_requested_quality: Mutex<Option<u64>>,
}

impl DashRequestedQualityMetrics {
    fn new(labels: &DashNetworkMetricLabels) -> Self {
        let metrics = get_metrics();
        let base_label_names = DashNetworkMetricLabels::names();
        let base_label_values = labels.values();

        Self {
            switches_up_total: metrics
                .get_or_create_labelled_gauge(
                    "dash_requested_quality_switches_total",
                    "Total number of requested DASH quality switches by direction",
                    &[base_label_names[0], base_label_names[1], "direction"],
                    &[base_label_values[0], base_label_values[1], "up"],
                )
                .expect("failed to create DASH requested quality up-switch metric"),
            switches_down_total: metrics
                .get_or_create_labelled_gauge(
                    "dash_requested_quality_switches_total",
                    "Total number of requested DASH quality switches by direction",
                    &[base_label_names[0], base_label_names[1], "direction"],
                    &[base_label_values[0], base_label_values[1], "down"],
                )
                .expect("failed to create DASH requested quality down-switch metric"),
            last_requested_quality: Mutex::new(None),
        }
    }

    fn record_requested_quality(&self, quality: u64) {
        let mut last_requested_quality = self.last_requested_quality.lock().unwrap();
        if let Some(previous_quality) = *last_requested_quality {
            match quality.cmp(&previous_quality) {
                std::cmp::Ordering::Greater => self.switches_up_total.inc(),
                std::cmp::Ordering::Less => self.switches_down_total.inc(),
                std::cmp::Ordering::Equal => {}
            }
        }
        *last_requested_quality = Some(quality);
    }

    fn reset(&self) {
        self.switches_up_total.set(0);
        self.switches_down_total.set(0);
        *self.last_requested_quality.lock().unwrap() = None;
    }
}

#[derive(Clone)]
struct DashRepresentationMetrics {
    segments_total: IntGauge,
}

impl DashRepresentationMetrics {
    fn new(labels: &DashNetworkMetricLabels, representation_id: &str) -> Self {
        let metrics = get_metrics();
        let base_label_names = DashNetworkMetricLabels::names();
        let base_label_values = labels.values();

        Self {
            segments_total: metrics
                .get_or_create_labelled_gauge(
                    "dash_segments_total",
                    DASH_SEGMENTS_TOTAL_HELP,
                    &[
                        base_label_names[0],
                        base_label_names[1],
                        "representation_id",
                    ],
                    &[
                        base_label_values[0],
                        base_label_values[1],
                        representation_id,
                    ],
                )
                .expect("failed to create DASH segment total metric"),
        }
    }

    fn reset(&self) {
        self.segments_total.set(0);
    }
}

#[derive(Clone)]
struct DashRobustnessMetrics {
    labels: DashNetworkMetricLabels,
    empty_segments_total: IntGauge,
    download_errors_total: IntGauge,
    representation_metrics: DashMap<String, DashRepresentationMetrics>,
}

impl DashRobustnessMetrics {
    fn new(labels: &DashNetworkMetricLabels) -> Self {
        let metrics = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();

        Self {
            labels: labels.clone(),
            empty_segments_total: metrics
                .get_or_create_labelled_gauge(
                    "dash_empty_segments_total",
                    DASH_EMPTY_SEGMENTS_TOTAL_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH empty segment metric"),
            download_errors_total: metrics
                .get_or_create_labelled_gauge(
                    "dash_download_errors_total",
                    DASH_DOWNLOAD_ERRORS_TOTAL_HELP,
                    &label_names,
                    &label_values,
                )
                .expect("failed to create DASH download error metric"),
            representation_metrics: DashMap::new(),
        }
    }

    fn record_segment(&self, representation_id: &str) {
        let metrics = self.get_or_create_representation_metrics(representation_id);
        metrics.segments_total.inc();
    }

    fn record_empty_segment(&self) {
        self.empty_segments_total.inc();
    }

    fn record_download_error(&self) {
        self.download_errors_total.inc();
    }

    fn reset(&self) {
        self.empty_segments_total.set(0);
        self.download_errors_total.set(0);
        for metrics in self.representation_metrics.iter() {
            metrics.value().reset();
        }
        self.representation_metrics.clear();
    }

    fn get_or_create_representation_metrics(
        &self,
        representation_id: &str,
    ) -> DashRepresentationMetrics {
        if let Some(metrics) = self.representation_metrics.get(representation_id) {
            return metrics.clone();
        }

        match self
            .representation_metrics
            .entry(representation_id.to_string())
        {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let metrics = DashRepresentationMetrics::new(&self.labels, representation_id);
                entry.insert(metrics.clone());
                metrics
            }
        }
    }
}

fn ms_to_us_gauge(value_ms: f64) -> i64 {
    (value_ms.max(0.0) * 1000.0)
        .round()
        .clamp(0.0, i64::MAX as f64) as i64
}

fn signed_ms_to_us_gauge(value_ms: f64) -> i64 {
    (value_ms * 1000.0)
        .round()
        .clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

fn u64_to_i64_gauge(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn parse_dash_representation_quality(representation_id: &str) -> u64 {
    representation_id
        .split('_')
        .next_back()
        .and_then(|last_part| last_part.parse::<u64>().ok())
        .unwrap_or_default()
}

pub struct DashIngress {
    url: String,
    group_map: GroupMap,
    abr_mode: AbrModeHandle,
    // pub stream_manager: Weak<StreamManager>,
    pub processing_pipeline: Arc<ProcessingPipeline>,
    pub runtime: Arc<Mutex<Option<Runtime>>>,
}
crate::log_drop!(DashIngress);

impl DashIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let url = stream_manager.http_url.read().unwrap().clone();
        if url.is_none() {
            error!("URL is empty");
            return;
        }

        let runtime = Arc::clone(&processing_pipeline.runtime);
        let ingress = Arc::new(Self {
            url: url.unwrap(),
            group_map: Arc::new(RwLock::new(HashMap::new())),
            abr_mode: stream_manager.abr_mode_handle(),
            // stream_manager: Arc::downgrade(&stream_manager),
            processing_pipeline,
            runtime,
        });

        // Keep a reference to ourselves in the StreamManager
        stream_manager.set_dash_ingress(ingress);
    }

    pub fn stop(&self) {
        info!("Stopping DASH ingress");

        // Stop all active players
        let mut group_map = self.group_map.write().unwrap();
        for (
            group_id,
            (
                handle,
                player,
                network_metrics,
                abr_metrics,
                timing_metrics,
                requested_quality_metrics,
                robustness_metrics,
            ),
        ) in group_map.drain()
        {
            info!("Stopping DASH player for group_id '{}'", group_id);
            player.stop();
            handle.abort();
            network_metrics.reset();
            abr_metrics.reset();
            timing_metrics.reset();
            requested_quality_metrics.reset();
            robustness_metrics.reset();
            self.processing_pipeline.remove_stream(group_id.clone());
        }

        // Clear the group map
        group_map.clear();
    }

    pub fn spawn_group(&self, group_id: String) {
        if let Some(rt) = self.runtime.lock().unwrap().as_ref() {
            self.processing_pipeline
                .activate_stream_for_transport(ReceiverTransport::Dash, group_id.clone());
            rt.block_on(async {
                // Wait 1 second, then spawn. This makes sure that all the representations are available in the backend.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                self.spawn_group_tokio(group_id);
            });
        }
    }

    fn spawn_group_tokio(&self, group_id: String) {
        if self.group_map.read().unwrap().contains_key(&group_id) {
            debug!("DASH player for group_id '{}' already exists", group_id);
            return;
        }

        debug!("Spawning DASH player for group_id '{}'", group_id);

        let stream_id = group_id.to_string();
        let mpd_url = format!("{}/dash/{}.mpd", &self.url, group_id);
        let pipeline = Arc::clone(&self.processing_pipeline);
        let lifecycle_pipeline = Arc::clone(&self.processing_pipeline);
        let abr_mode = self.abr_mode.clone();
        let group_id_clone = group_id.clone(); // clone for move into task
        let group_map = Arc::clone(&self.group_map); // Clone for move into task
        let labels = DashNetworkMetricLabels::new(&group_id, &mpd_url);
        let network_metrics = Arc::new(DashNetworkMetrics::new(&labels));
        let abr_metrics = Arc::new(DashAbrMetrics::new(&labels));
        let timing_metrics = Arc::new(DashPlayerTimingMetrics::new(&labels));
        let requested_quality_metrics = Arc::new(DashRequestedQualityMetrics::new(&labels));
        let robustness_metrics = Arc::new(DashRobustnessMetrics::new(&labels));
        let callback_network_metrics = Arc::clone(&network_metrics);
        let callback_abr_metrics = Arc::clone(&abr_metrics);
        let callback_timing_metrics = Arc::clone(&timing_metrics);
        let callback_requested_quality_metrics = Arc::clone(&requested_quality_metrics);
        let callback_robustness_metrics = Arc::clone(&robustness_metrics);

        let callback = Arc::new(move |event: DashEvent| {
            let cb_pipeline = Arc::clone(&pipeline);
            let cb_stream_id = stream_id.clone();
            let cb_group_id = group_id_clone.clone();
            let cb_network_metrics = Arc::clone(&callback_network_metrics);
            let cb_abr_metrics = Arc::clone(&callback_abr_metrics);
            let cb_timing_metrics = Arc::clone(&callback_timing_metrics);
            let cb_requested_quality_metrics = Arc::clone(&callback_requested_quality_metrics);
            let cb_robustness_metrics = Arc::clone(&callback_robustness_metrics);

            tokio::spawn(async move {
                match event {
                    DashEvent::Segment {
                        data,
                        content_type,
                        representation_id,
                        segment_number,
                        url,
                        playback_rate,
                        network_stats,
                        abr_stats,
                        timing_stats,
                        ..
                    } => {
                        cb_network_metrics.record(&network_stats);
                        let dash_clock_source = network_stats
                            .origin_clock_source_id
                            .as_ref()
                            .map(|source_id| {
                                ClockSourceKey::with_server_id(ClockDomain::Dash, source_id)
                            })
                            .unwrap_or_else(|| ClockSourceKey::for_transport(ClockDomain::Dash));
                        if let Some(origin_clock_offset_ms) = network_stats.origin_clock_offset_ms {
                            let offset_us = signed_ms_to_us_gauge(origin_clock_offset_ms);
                            cb_pipeline.observe_clock_offset_us_for_source(
                                dash_clock_source.clone(),
                                ClockSampleTrust::HighRtt,
                                offset_us,
                            );
                        }
                        cb_timing_metrics.record(&timing_stats);
                        debug!(
                            "DASH [{} - {}] - segment {} (type: {}, rate: {}) size: {} bytes",
                            cb_group_id,
                            representation_id,
                            segment_number,
                            content_type,
                            playback_rate,
                            data.len()
                        );

                        if url.ends_with("init.mp4") {
                            return;
                        }

                        cb_abr_metrics.record(&abr_stats);
                        cb_requested_quality_metrics.record_requested_quality(
                            parse_dash_representation_quality(&representation_id),
                        );
                        cb_robustness_metrics.record_segment(&representation_id);

                        //info!(url);
                        //info!("First 16 bytes: {:?}", &data[..16.min(data.len())]);

                        // Use fast mdat extractor
                        let mdat_boxes = match extract_mdat_boxes(&data) {
                            Ok(boxes) => boxes,
                            Err(err) => {
                                warn!("Failed to parse mdat boxes: {}", err);
                                return;
                            }
                        };

                        if mdat_boxes.is_empty() {
                            debug!("No mdat boxes found in segment {}", segment_number);
                            return;
                        }

                        let quality = parse_dash_representation_quality(&representation_id);

                        for mdat in mdat_boxes {
                            let mdat_data = mdat.data;
                            if mdat_data.is_empty() {
                                debug!("Empty mdat box found");
                                continue;
                            }

                            if !mdat_data.starts_with(PCF_MAGIC) {
                                warn!(
                                    "Skipping non-PCF DASH mdat payload with {} bytes",
                                    mdat_data.len()
                                );
                                continue;
                            }

                            cb_pipeline.ingest_data_for_source(
                                dash_clock_source.clone(),
                                cb_stream_id.clone(),
                                quality,
                                0,
                                0,
                                mdat_data,
                            );
                        }
                    }
                    DashEvent::EmptySegment { segment_number } => {
                        cb_robustness_metrics.record_empty_segment();
                        debug!("DASH [{}] EmptySegment: {}", cb_group_id, segment_number);
                        cb_pipeline.empty_frame(cb_stream_id.clone());
                    }
                    DashEvent::Info(msg) => info!("DASH [{}] Info: {}", cb_group_id, msg),
                    DashEvent::Warning(msg) => warn!("DASH [{}] Warning: {}", cb_group_id, msg),
                    DashEvent::DownloadError { url, reason } => {
                        cb_robustness_metrics.record_download_error();
                        error!("DASH [{}] DownloadError: {} - {}", cb_group_id, url, reason)
                    }
                }
            });
        });

        // Spawn task to create player and its own task
        tokio::spawn(async move {
            match DashPlayer::new(&mpd_url, callback).await {
                Ok(mut player) => {
                    player.set_abr_mode_handle(abr_mode);
                    player.set_target_latency(0.001).await;
                    let player = Arc::new(player);
                    let group_id_clone = group_id.clone();
                    let player_clone = Arc::clone(&player);
                    let start_failure_pipeline = Arc::clone(&lifecycle_pipeline);

                    let handle = tokio::spawn(async move {
                        if let Err(e) = player_clone.start().await {
                            error!("DASH [{}] Failed to start player: {}", group_id_clone, e);
                            start_failure_pipeline.remove_stream(group_id_clone.clone());
                        }
                    });

                    group_map.write().unwrap().insert(
                        group_id,
                        (
                            handle,
                            player,
                            network_metrics,
                            abr_metrics,
                            timing_metrics,
                            requested_quality_metrics,
                            robustness_metrics,
                        ),
                    );
                }
                Err(e) => {
                    error!("DASH [{}] Failed to create player: {}", group_id, e);
                    lifecycle_pipeline.remove_stream(group_id.clone());
                }
            }
        });
    }

    pub fn set_fetching_enabled(&self, group_id: &str, enabled: bool) {
        if let Some((_, player, _, _, _, _, _)) = self.group_map.read().unwrap().get(group_id) {
            player.set_fetching_enabled(enabled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn ensure_metrics_initialized() {
        if catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok() {
            return;
        }

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = metrics::MetricsBuilder::new()
                .add_label("mode", "client-test")
                .build();
        }));

        assert!(
            catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok(),
            "failed to initialize global metrics for receiver tests"
        );
    }

    #[test]
    fn dash_network_metrics_record_estimated_rtt() {
        ensure_metrics_initialized();

        let labels =
            DashNetworkMetricLabels::new("group_1", "https://dash.example.com/live/group_1.mpd");
        let metrics = DashNetworkMetrics::new(&labels);
        metrics.record(&DashNetworkStats {
            ttfb_ms: 40.0,
            server_wait_ms: Some(10),
            estimated_one_way_latency_ms: Some(15.0),
            estimated_rtt_ms: Some(30.0),
            serving_hop_clock_offset_ms: Some(20.0),
            origin_clock_offset_ms: Some(5.0),
            origin_clock_source_id: None,
        });

        let registry = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();
        let rtt = registry
            .get_or_create_labelled_gauge(
                "dash_network_rtt_us",
                "Estimated DASH network RTT in microseconds, derived from HTTP TTFB minus backend wait time",
                &label_names,
                &label_values,
            )
            .unwrap();
        let one_way = registry
            .get_or_create_labelled_gauge(
                "dash_network_one_way_latency_us",
                "Estimated DASH client-to-server one-way network latency in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();
        let ttfb = registry
            .get_or_create_labelled_gauge(
                "dash_network_ttfb_us",
                "HTTP time-to-first-byte for DASH segment requests in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();
        let backend_wait = registry
            .get_or_create_labelled_gauge(
                "dash_backend_wait_us",
                "Server-reported backend wait time for DASH segment requests in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();
        let serving_hop_clock_offset = registry
            .get_or_create_labelled_gauge(
                "dash_serving_hop_clock_offset_us",
                "Estimated serving-hop-minus-client wall-clock offset for DASH in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();
        let origin_clock_offset = registry
            .get_or_create_labelled_gauge(
                "dash_origin_clock_offset_us",
                "Estimated origin-server-minus-client wall-clock offset for DASH in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();
        let legacy_clock_offset = registry
            .get_or_create_labelled_gauge(
                "dash_network_clock_offset_us",
                "Estimated origin-server-minus-client wall-clock offset for DASH in microseconds",
                &label_names,
                &label_values,
            )
            .unwrap();

        assert_eq!(rtt.get(), 30_000);
        assert_eq!(one_way.get(), 15_000);
        assert_eq!(ttfb.get(), 40_000);
        assert_eq!(backend_wait.get(), 10_000);
        assert_eq!(serving_hop_clock_offset.get(), 20_000);
        assert_eq!(origin_clock_offset.get(), 5_000);
        assert_eq!(legacy_clock_offset.get(), 5_000);

        metrics.record(&DashNetworkStats {
            ttfb_ms: 35.0,
            server_wait_ms: Some(12),
            estimated_one_way_latency_ms: Some(11.5),
            estimated_rtt_ms: Some(23.0),
            serving_hop_clock_offset_ms: Some(-2.5),
            origin_clock_offset_ms: Some(-7.0),
            origin_clock_source_id: None,
        });

        assert_eq!(serving_hop_clock_offset.get(), -2_500);
        assert_eq!(origin_clock_offset.get(), -7_000);
        assert_eq!(legacy_clock_offset.get(), -7_000);

        metrics.reset();
        assert_eq!(rtt.get(), 0);
        assert_eq!(one_way.get(), 0);
        assert_eq!(ttfb.get(), 0);
        assert_eq!(backend_wait.get(), 0);
        assert_eq!(serving_hop_clock_offset.get(), 0);
        assert_eq!(origin_clock_offset.get(), 0);
        assert_eq!(legacy_clock_offset.get(), 0);
    }

    #[test]
    fn dash_robustness_metrics_record_segment_empty_and_error_events() {
        ensure_metrics_initialized();

        let labels =
            DashNetworkMetricLabels::new("group_1", "https://dash.example.com/live/group_1.mpd");
        let metrics = DashRobustnessMetrics::new(&labels);
        metrics.record_segment("video_2");
        metrics.record_segment("video_2");
        metrics.record_segment("video_1");
        metrics.record_empty_segment();
        metrics.record_download_error();

        let registry = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();
        let empty_segments_total = registry
            .get_or_create_labelled_gauge(
                "dash_empty_segments_total",
                DASH_EMPTY_SEGMENTS_TOTAL_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let download_errors_total = registry
            .get_or_create_labelled_gauge(
                "dash_download_errors_total",
                DASH_DOWNLOAD_ERRORS_TOTAL_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let representation_two_segments_total = registry
            .get_or_create_labelled_gauge(
                "dash_segments_total",
                DASH_SEGMENTS_TOTAL_HELP,
                &[label_names[0], label_names[1], "representation_id"],
                &[label_values[0], label_values[1], "video_2"],
            )
            .unwrap();
        let representation_one_segments_total = registry
            .get_or_create_labelled_gauge(
                "dash_segments_total",
                DASH_SEGMENTS_TOTAL_HELP,
                &[label_names[0], label_names[1], "representation_id"],
                &[label_values[0], label_values[1], "video_1"],
            )
            .unwrap();

        assert_eq!(empty_segments_total.get(), 1);
        assert_eq!(download_errors_total.get(), 1);
        assert_eq!(representation_two_segments_total.get(), 2);
        assert_eq!(representation_one_segments_total.get(), 1);

        metrics.reset();
        assert_eq!(empty_segments_total.get(), 0);
        assert_eq!(download_errors_total.get(), 0);
        assert_eq!(representation_two_segments_total.get(), 0);
        assert_eq!(representation_one_segments_total.get(), 0);
    }

    #[test]
    fn dash_abr_metrics_record_and_reset_decision_state() {
        ensure_metrics_initialized();

        let labels =
            DashNetworkMetricLabels::new("group_1", "https://dash.example.com/live/group_1.mpd");
        let metrics = DashAbrMetrics::new(&labels);
        metrics.record(&DashAbrStats {
            estimated_bandwidth_bps: 8_000_000,
            bandwidth_budget_bps: 7_200_000,
            risk_adjusted_bandwidth_budget_bps: 6_400_000,
            last_throughput_sample_bps: 9_100_000,
            last_whole_request_throughput_sample_bps: 4_600_000,
            requested_representation_bitrate_bps: 5_500_000,
        });

        let registry = get_metrics();
        let label_names = DashNetworkMetricLabels::names();
        let label_values = labels.values();

        let estimated_bandwidth = registry
            .get_or_create_labelled_gauge(
                "dash_estimated_bandwidth_bps",
                DASH_ESTIMATED_BANDWIDTH_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let bandwidth_budget = registry
            .get_or_create_labelled_gauge(
                "dash_bandwidth_budget_bps",
                DASH_BANDWIDTH_BUDGET_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let risk_adjusted_budget = registry
            .get_or_create_labelled_gauge(
                "dash_risk_adjusted_bandwidth_budget_bps",
                DASH_RISK_ADJUSTED_BANDWIDTH_BUDGET_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let last_throughput_sample = registry
            .get_or_create_labelled_gauge(
                "dash_last_throughput_sample_bps",
                DASH_LAST_THROUGHPUT_SAMPLE_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let last_whole_request_throughput_sample = registry
            .get_or_create_labelled_gauge(
                "dash_last_whole_request_throughput_sample_bps",
                DASH_LAST_WHOLE_REQUEST_THROUGHPUT_SAMPLE_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();
        let requested_representation_bitrate = registry
            .get_or_create_labelled_gauge(
                "dash_requested_representation_bitrate_bps",
                DASH_REQUESTED_REPRESENTATION_BITRATE_BPS_HELP,
                &label_names,
                &label_values,
            )
            .unwrap();

        assert_eq!(estimated_bandwidth.get(), 8_000_000);
        assert_eq!(bandwidth_budget.get(), 7_200_000);
        assert_eq!(risk_adjusted_budget.get(), 6_400_000);
        assert_eq!(last_throughput_sample.get(), 9_100_000);
        assert_eq!(last_whole_request_throughput_sample.get(), 4_600_000);
        assert_eq!(requested_representation_bitrate.get(), 5_500_000);

        metrics.reset();

        assert_eq!(estimated_bandwidth.get(), 0);
        assert_eq!(bandwidth_budget.get(), 0);
        assert_eq!(risk_adjusted_budget.get(), 0);
        assert_eq!(last_throughput_sample.get(), 0);
        assert_eq!(last_whole_request_throughput_sample.get(), 0);
        assert_eq!(requested_representation_bitrate.get(), 0);
    }
}

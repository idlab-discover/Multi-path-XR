use circular_buffer::CircularBuffer;
use dashmap::{mapref::entry::Entry, DashMap};
use metrics::{get_metrics, Metrics as AppMetrics};
use prometheus::IntGauge;
use shared_utils::types::FrameData;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

#[derive(Clone)]
struct PerStreamMetrics {
    frames_received_total: IntGauge,
    send_to_receive_time_diff: IntGauge,
    frames_consumed_total: IntGauge,
    send_to_consume_time_diff: IntGauge,
    receive_to_consume_time_diff: IntGauge,
    rendered_point_count: IntGauge,
}

impl PerStreamMetrics {
    fn new(metrics: &AppMetrics, stream_id: &str) -> Self {
        Self {
            frames_received_total: metrics
                .get_or_create_labelled_gauge(
                    "frames_received_total_per_stream",
                    "Total number of frames received per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("frames_received_total_per_stream"),
            send_to_receive_time_diff: metrics
                .get_or_create_labelled_gauge(
                    "send_to_receive_time_diff_per_stream",
                    "Difference (us) between send time and receive time of a frame per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("send_to_receive_time_diff_per_stream"),
            frames_consumed_total: metrics
                .get_or_create_labelled_gauge(
                    "frames_consumed_total_per_stream",
                    "Total number of frames consumed per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("frames_consumed_total_per_stream"),
            send_to_consume_time_diff: metrics
                .get_or_create_labelled_gauge(
                    "send_to_consume_time_diff_per_stream",
                    "Difference (us) between send time and consume time of a frame per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("send_to_consume_time_diff_per_stream"),
            receive_to_consume_time_diff: metrics
                .get_or_create_labelled_gauge(
                    "receive_to_consume_time_diff_per_stream",
                    "Difference (us) between receive time and consume time of a frame per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("receive_to_consume_time_diff_per_stream"),
            rendered_point_count: metrics
                .get_or_create_labelled_gauge(
                    "rendered_point_count_per_stream",
                    "Number of points in the last consumed frame per stream",
                    &["stream_id"],
                    &[stream_id],
                )
                .expect("rendered_point_count_per_stream"),
        }
    }

    fn reset(&self) {
        self.frames_received_total.set(0);
        self.frames_consumed_total.set(0);
        self.reset_last_values();
    }

    fn reset_last_values(&self) {
        self.send_to_receive_time_diff.set(0);
        self.send_to_consume_time_diff.set(0);
        self.receive_to_consume_time_diff.set(0);
        self.rendered_point_count.set(0);
    }
}

#[derive(Clone)]
struct PerQualityRenderMetrics {
    rendered_frames_total: IntGauge,
    rendered_time_us_total: IntGauge,
}

impl PerQualityRenderMetrics {
    fn new(metrics: &AppMetrics, quality: u32) -> Self {
        let quality_label = quality.to_string();
        let quality_value = quality_label.as_str();
        Self {
            rendered_frames_total: metrics
                .get_or_create_labelled_gauge(
                    "rendered_frames_total_by_quality",
                    "Total number of consumed frames by rendered quality",
                    &["quality"],
                    &[quality_value],
                )
                .expect("rendered_frames_total_by_quality"),
            rendered_time_us_total: metrics
                .get_or_create_labelled_gauge(
                    "rendered_time_us_total",
                    "Accumulated rendered time in microseconds by quality",
                    &["quality"],
                    &[quality_value],
                )
                .expect("rendered_time_us_total"),
        }
    }

    fn reset(&self) {
        self.rendered_frames_total.set(0);
        self.rendered_time_us_total.set(0);
    }
}

#[derive(Clone)]
struct RenderedQualitySwitchMetrics {
    up_total: IntGauge,
    down_total: IntGauge,
}

impl RenderedQualitySwitchMetrics {
    fn new(metrics: &AppMetrics) -> Self {
        Self {
            up_total: metrics
                .get_or_create_labelled_gauge(
                    "rendered_quality_switches_total",
                    "Total number of rendered quality switches by direction",
                    &["direction"],
                    &["up"],
                )
                .expect("rendered_quality_switches_total up"),
            down_total: metrics
                .get_or_create_labelled_gauge(
                    "rendered_quality_switches_total",
                    "Total number of rendered quality switches by direction",
                    &["direction"],
                    &["down"],
                )
                .expect("rendered_quality_switches_total down"),
        }
    }

    fn reset(&self) {
        self.up_total.set(0);
        self.down_total.set(0);
    }

    fn record_switch(&self, previous_quality: u32, current_quality: u32) {
        match current_quality.cmp(&previous_quality) {
            std::cmp::Ordering::Greater => self.up_total.inc(),
            std::cmp::Ordering::Less => self.down_total.inc(),
            std::cmp::Ordering::Equal => {}
        }
    }
}

#[derive(Clone, Copy)]
struct RenderedQoEState {
    last_consume_time_us: u64,
    last_quality: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverTransport {
    Dash,
    Flute,
    Moq,
}

impl ReceiverTransport {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dash => "dash",
            Self::Flute => "flute",
            Self::Moq => "moq",
        }
    }
}

#[derive(Clone)]
struct TransportLifecycleMetrics {
    join_events_total: IntGauge,
    leave_events_total: IntGauge,
    rejoin_events_total: IntGauge,
    last_join_latency_us: IntGauge,
    join_latency_sum_us: IntGauge,
    join_latency_samples_total: IntGauge,
}

impl TransportLifecycleMetrics {
    fn new(metrics: &AppMetrics, transport: ReceiverTransport) -> Self {
        let transport_label = transport.as_str();

        Self {
            join_events_total: metrics
                .get_or_create_labelled_gauge(
                    "join_events_total",
                    "Total number of successful visible joins by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("join_events_total"),
            leave_events_total: metrics
                .get_or_create_labelled_gauge(
                    "leave_events_total",
                    "Total number of visible leaves by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("leave_events_total"),
            rejoin_events_total: metrics
                .get_or_create_labelled_gauge(
                    "rejoin_events_total",
                    "Total number of successful visible rejoins by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("rejoin_events_total"),
            last_join_latency_us: metrics
                .get_or_create_labelled_gauge(
                    "last_join_latency_us",
                    "Latency in microseconds from join start to the first usable consumed frame by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("last_join_latency_us"),
            join_latency_sum_us: metrics
                .get_or_create_labelled_gauge(
                    "join_latency_sum_us",
                    "Accumulated join latency in microseconds from join start to the first usable consumed frame by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("join_latency_sum_us"),
            join_latency_samples_total: metrics
                .get_or_create_labelled_gauge(
                    "join_latency_samples_total",
                    "Total number of join-latency samples recorded by transport",
                    &["transport"],
                    &[transport_label],
                )
                .expect("join_latency_samples_total"),
        }
    }

    fn reset(&self) {
        self.join_events_total.set(0);
        self.leave_events_total.set(0);
        self.rejoin_events_total.set(0);
        self.last_join_latency_us.set(0);
        self.join_latency_sum_us.set(0);
        self.join_latency_samples_total.set(0);
    }
}

struct ReceiverLifecycleMetrics {
    dash: TransportLifecycleMetrics,
    flute: TransportLifecycleMetrics,
    moq: TransportLifecycleMetrics,
}

impl ReceiverLifecycleMetrics {
    fn new(metrics: &AppMetrics) -> Self {
        Self {
            dash: TransportLifecycleMetrics::new(metrics, ReceiverTransport::Dash),
            flute: TransportLifecycleMetrics::new(metrics, ReceiverTransport::Flute),
            moq: TransportLifecycleMetrics::new(metrics, ReceiverTransport::Moq),
        }
    }

    fn for_transport(&self, transport: ReceiverTransport) -> &TransportLifecycleMetrics {
        match transport {
            ReceiverTransport::Dash => &self.dash,
            ReceiverTransport::Flute => &self.flute,
            ReceiverTransport::Moq => &self.moq,
        }
    }

    fn reset(&self) {
        self.dash.reset();
        self.flute.reset();
        self.moq.reset();
    }
}

#[derive(Clone, Copy)]
struct PendingJoinState {
    transport: ReceiverTransport,
    started_at_us: u64,
    is_rejoin: bool,
}

fn current_time_us() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
}

const FREEZE_THRESHOLD_INTERVAL_MULTIPLIER: u64 = 3;
const FREEZE_EXPECTED_INTERVAL_UPDATE_MAX_MULTIPLIER: u64 = 2;

#[derive(Clone)]
struct FreezeMetrics {
    freeze_events_total: IntGauge,
    freeze_time_us_total: IntGauge,
    last_freeze_duration_us: IntGauge,
}

impl FreezeMetrics {
    fn new(metrics: &AppMetrics) -> Self {
        Self {
            freeze_events_total: metrics
                .get_or_create_gauge(
                    "freeze_events_total",
                    "Total number of completed freeze events observed at the final consume point",
                )
                .expect("freeze_events_total"),
            freeze_time_us_total: metrics
                .get_or_create_gauge(
                    "freeze_time_us_total",
                    "Accumulated freeze time in microseconds beyond the expected frame interval",
                )
                .expect("freeze_time_us_total"),
            last_freeze_duration_us: metrics
                .get_or_create_gauge(
                    "last_freeze_duration_us",
                    "Duration in microseconds of the last completed freeze beyond the expected frame interval",
                )
                .expect("last_freeze_duration_us"),
        }
    }

    fn record_freeze(&self, freeze_duration_us: u64) {
        let freeze_duration_gauge = freeze_duration_us.min(i64::MAX as u64) as i64;
        self.freeze_events_total.inc();
        self.freeze_time_us_total.add(freeze_duration_gauge);
        self.last_freeze_duration_us.set(freeze_duration_gauge);
    }

    fn reset(&self) {
        self.freeze_events_total.set(0);
        self.freeze_time_us_total.set(0);
        self.last_freeze_duration_us.set(0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamFreezeState {
    last_consume_time_us: u64,
    last_presentation_time_us: u64,
    expected_frame_interval_us: Option<u64>,
}

fn update_expected_frame_interval_us(
    expected_frame_interval_us: Option<u64>,
    presentation_delta_us: Option<u64>,
) -> Option<u64> {
    let presentation_delta_us = presentation_delta_us.filter(|delta_us| *delta_us > 0);

    match (expected_frame_interval_us, presentation_delta_us) {
        (None, Some(presentation_delta_us)) => Some(presentation_delta_us),
        (Some(expected_frame_interval_us), Some(presentation_delta_us))
            if presentation_delta_us
                <= expected_frame_interval_us
                    .saturating_mul(FREEZE_EXPECTED_INTERVAL_UPDATE_MAX_MULTIPLIER) =>
        {
            Some(expected_frame_interval_us.saturating_add(presentation_delta_us) / 2)
        }
        (Some(expected_frame_interval_us), _) => Some(expected_frame_interval_us),
        (None, None) => None,
    }
}

fn observe_freeze_state(
    previous_state: Option<StreamFreezeState>,
    current_consume_time_us: u64,
    current_presentation_time_us: u64,
) -> (StreamFreezeState, Option<u64>) {
    let freeze_duration_us = previous_state.and_then(|previous_state| {
        previous_state
            .expected_frame_interval_us
            .and_then(|expected_frame_interval_us| {
                let consume_gap_us =
                    current_consume_time_us.saturating_sub(previous_state.last_consume_time_us);
                let freeze_threshold_us =
                    expected_frame_interval_us.saturating_mul(FREEZE_THRESHOLD_INTERVAL_MULTIPLIER);

                (consume_gap_us > freeze_threshold_us)
                    .then_some(consume_gap_us.saturating_sub(expected_frame_interval_us))
            })
    });

    let expected_frame_interval_us = previous_state.and_then(|previous_state| {
        let presentation_delta_us = current_presentation_time_us
            .checked_sub(previous_state.last_presentation_time_us)
            .filter(|delta_us| *delta_us > 0);

        update_expected_frame_interval_us(
            previous_state.expected_frame_interval_us,
            presentation_delta_us,
        )
    });

    (
        StreamFreezeState {
            last_consume_time_us: current_consume_time_us,
            last_presentation_time_us: current_presentation_time_us,
            expected_frame_interval_us,
        },
        freeze_duration_us,
    )
}

pub struct Storage {
    pub metrics: AppMetrics,
    buffers: RwLock<HashMap<String, Arc<RwLock<CircularBuffer<30, FrameData>>>>>,
    latest_presentation_time: RwLock<HashMap<String, u64>>,
    last_consumed_point_counts: RwLock<HashMap<String, u64>>,
    deactivated_streams: RwLock<HashSet<String>>,
    per_stream_metrics: DashMap<String, PerStreamMetrics>,
    per_quality_render_metrics: DashMap<u32, PerQualityRenderMetrics>,
    rendered_quality_switch_metrics: RenderedQualitySwitchMetrics,
    rendered_qoe_state: Mutex<Option<RenderedQoEState>>,
    lifecycle_metrics: ReceiverLifecycleMetrics,
    pending_stream_joins: DashMap<String, PendingJoinState>,
    known_joined_streams: DashMap<String, ReceiverTransport>,
    active_joined_streams: DashMap<String, ReceiverTransport>,
    freeze_metrics: FreezeMetrics,
    per_stream_freeze_state: DashMap<String, StreamFreezeState>,
    pub reception_time_flute: IntGauge,
    pub frames_consumed_total: IntGauge,
    pub frames_received_total: IntGauge,
    pub frames_skipped_total: IntGauge,
    pub frames_dropped_before_decode_total: IntGauge,
    pub predecode_frames_in_flight: IntGauge,
    pub predecode_frames_pending: IntGauge,
    pub current_backlog: IntGauge,
    pub send_to_receive_time_diff: IntGauge,
    pub send_to_consume_time_diff: IntGauge,
    pub receive_to_consume_time_diff: IntGauge,
    pub point_count_metric: IntGauge,
    pub decode_time: IntGauge,
    pub total_point_count: IntGauge,
    pub quality_metric: IntGauge,
    pub rendered_quality_tier: IntGauge,
}
crate::log_drop!(Storage);

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    pub fn new() -> Self {
        let metrics = get_metrics();

        // Example metrics:
        let reception_time_flute = metrics
            .get_or_create_gauge(
                "reception_time_flute",
                "Time (ms) it took to receive a FLUTE object",
            )
            .expect("Failed to create reception_time_flute gauge");

        let frames_consumed_total = metrics
            .get_or_create_gauge(
                "frames_consumed_total",
                "Total number of frames consumed properly",
            )
            .expect("Failed to create frames_consumed_total gauge");

        let frames_received_total = metrics
            .get_or_create_gauge(
                "frames_received_total",
                "Total number of frames that have been received",
            )
            .expect("Failed to create frames_received_total gauge");

        let frames_skipped_total = metrics
            .get_or_create_gauge(
                "frames_skipped_total",
                "Total number of frames skipped due to backlog",
            )
            .expect("Failed to create frames_skipped_total gauge");

        let frames_dropped_before_decode_total = metrics
            .get_or_create_gauge(
                "frames_dropped_before_decode_total",
                "Total number of frames dropped before decoding",
            )
            .expect("Failed to create frames_dropped_before_decode_total gauge");

        let predecode_frames_in_flight = metrics
            .get_or_create_gauge(
                "predecode_frames_in_flight",
                "Current number of frames being decoded",
            )
            .expect("Failed to create predecode_frames_in_flight gauge");

        let predecode_frames_pending = metrics
            .get_or_create_gauge(
                "predecode_frames_pending",
                "Current number of compressed frames waiting to be decoded",
            )
            .expect("Failed to create predecode_frames_pending gauge");

        let current_backlog = metrics
            .get_or_create_gauge(
                "current_backlog",
                "Current maximum backlog across all streams",
            )
            .expect("Failed to create current_backlog gauge");

        let send_to_receive_time_diff = metrics
            .get_or_create_gauge(
                "send_to_receive_time_diff",
                "Difference (ms) between send time and receive time of a frame",
            )
            .expect("Failed to create send_to_receive_time_diff gauge");

        let send_to_consume_time_diff = metrics
            .get_or_create_gauge(
                "send_to_consume_time_diff",
                "Difference (ms) between send time and consume time of a frame",
            )
            .expect("Failed to create send_to_consume_time_diff gauge");

        let receive_to_consume_time_diff = metrics
            .get_or_create_gauge(
                "receive_to_consume_time_diff",
                "Difference (ms) between receive time and consume time of a frame",
            )
            .expect("Failed to create receive_to_consume_time_diff gauge");

        let point_count_metric = metrics
            .get_or_create_gauge(
                "point_count_metric",
                "Number of points in the last consumed frame",
            )
            .expect("Failed to create point_count_metric gauge");

        let decode_time = metrics
            .get_or_create_gauge("decoding_time", "Time taken to decode a frame")
            .unwrap();

        let total_point_count = metrics
            .get_or_create_gauge(
                "total_point_count",
                "Total concurrent point count across all streams",
            )
            .unwrap();

        let quality_metric = metrics
            .get_or_create_gauge("quality_metric", "Quality id of the stream")
            .expect("Failed to create quality_metric gauge");

        let rendered_quality_tier = metrics
            .get_or_create_gauge(
                "rendered_quality_tier",
                "Quality id of the last consumed frame",
            )
            .expect("Failed to create rendered_quality_tier gauge");
        let lifecycle_metrics = ReceiverLifecycleMetrics::new(&metrics);
        let rendered_quality_switch_metrics = RenderedQualitySwitchMetrics::new(&metrics);
        let freeze_metrics = FreezeMetrics::new(&metrics);

        Storage {
            metrics: metrics.clone(),
            buffers: RwLock::new(HashMap::new()),
            latest_presentation_time: RwLock::new(HashMap::new()),
            last_consumed_point_counts: RwLock::new(HashMap::new()),
            deactivated_streams: RwLock::new(HashSet::new()),
            per_stream_metrics: DashMap::new(),
            per_quality_render_metrics: DashMap::new(),
            rendered_quality_switch_metrics,
            rendered_qoe_state: Mutex::new(None),
            lifecycle_metrics,
            pending_stream_joins: DashMap::new(),
            known_joined_streams: DashMap::new(),
            active_joined_streams: DashMap::new(),
            freeze_metrics,
            per_stream_freeze_state: DashMap::new(),
            reception_time_flute,
            frames_consumed_total,
            frames_received_total,
            frames_skipped_total,
            frames_dropped_before_decode_total,
            predecode_frames_in_flight,
            predecode_frames_pending,
            current_backlog,
            send_to_receive_time_diff,
            send_to_consume_time_diff,
            receive_to_consume_time_diff,
            point_count_metric,
            decode_time,
            total_point_count,
            quality_metric,
            rendered_quality_tier,
        }
    }

    pub fn reset(&self) {
        {
            let mut buffers = self.buffers.write().unwrap();
            buffers.clear();
        }
        self.latest_presentation_time.write().unwrap().clear();
        self.last_consumed_point_counts.write().unwrap().clear();
        self.deactivated_streams.write().unwrap().clear();
        for metrics in self.per_stream_metrics.iter() {
            metrics.value().reset();
        }
        self.per_stream_metrics.clear();
        for metrics in self.per_quality_render_metrics.iter() {
            metrics.value().reset();
        }
        self.per_quality_render_metrics.clear();
        self.rendered_quality_switch_metrics.reset();
        *self.rendered_qoe_state.lock().unwrap() = None;
        self.lifecycle_metrics.reset();
        self.pending_stream_joins.clear();
        self.known_joined_streams.clear();
        self.active_joined_streams.clear();
        self.freeze_metrics.reset();
        self.per_stream_freeze_state.clear();

        // Reset all metrics
        self.reception_time_flute.set(0);
        self.frames_consumed_total.set(0);
        self.frames_received_total.set(0);
        self.frames_skipped_total.set(0);
        self.frames_dropped_before_decode_total.set(0);
        self.predecode_frames_in_flight.set(0);
        self.predecode_frames_pending.set(0);
        self.current_backlog.set(0);
        self.send_to_receive_time_diff.set(0);
        self.send_to_consume_time_diff.set(0);
        self.receive_to_consume_time_diff.set(0);
        self.point_count_metric.set(0);
        self.decode_time.set(0);
        self.total_point_count.set(0);
        self.quality_metric.set(0);
        self.rendered_quality_tier.set(0);
    }

    pub fn empty_frame(&self, stream_id: String) {
        // This does not insert anything, but does reset the last_consumed_point_counts to 0
        self.last_consumed_point_counts
            .write()
            .unwrap()
            .insert(stream_id, 0);
    }

    pub fn activate_stream(&self, stream_id: &str) {
        self.deactivated_streams.write().unwrap().remove(stream_id);
        let _ = self.get_or_create_per_stream_metrics(stream_id);
    }

    pub fn activate_stream_for_transport(&self, transport: ReceiverTransport, stream_id: &str) {
        self.activate_stream(stream_id);
        self.begin_join_tracking(transport, stream_id);
    }

    pub fn remove_stream(&self, stream_id: &str) {
        self.deactivated_streams
            .write()
            .unwrap()
            .insert(stream_id.to_string());

        self.pending_stream_joins.remove(stream_id);
        self.per_stream_freeze_state.remove(stream_id);
        if let Some((_, transport)) = self.active_joined_streams.remove(stream_id) {
            self.lifecycle_metrics
                .for_transport(transport)
                .leave_events_total
                .inc();
        }

        self.buffers.write().unwrap().remove(stream_id);
        self.latest_presentation_time
            .write()
            .unwrap()
            .remove(stream_id);
        let total_point_count = {
            let mut last_consumed_point_counts = self.last_consumed_point_counts.write().unwrap();
            last_consumed_point_counts.remove(stream_id);
            last_consumed_point_counts.values().sum::<u64>()
        };

        if let Some(metrics) = self.per_stream_metrics.get(stream_id) {
            let metrics = metrics.clone();
            metrics.reset_last_values();
        }

        self.total_point_count.set(total_point_count as i64);
    }

    pub fn insert_frame(&self, stream_id: String, mut frame: FrameData) {
        if self.is_stream_deactivated(&stream_id) {
            debug!("Dropping frame for deactivated stream_id: {}", stream_id);
            return;
        }

        // info!("Inserting frame with presentation time: {}", frame.presentation_time);
        // Check if the presentation time is 0
        if frame.presentation_time == 0 {
            // Overwrite the presentation time with the current time
            let current_time = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_micros() as u64,
                Err(_) => return,
            };
            frame.presentation_time = current_time;
        }

        let per_stream_metrics = self.get_or_create_per_stream_metrics(&stream_id);

        // ---- Out-of-order guard (per stream) ----
        let dropped_out_of_order = {
            let mut latest_map = self.latest_presentation_time.write().unwrap();
            let latest = latest_map.entry(stream_id.clone()).or_insert(0);
            if frame.presentation_time < *latest {
                true
            } else {
                // Accept equal or newer timestamps; track newest seen
                if frame.presentation_time > *latest {
                    *latest = frame.presentation_time;
                }
                false
            }
        };
        if dropped_out_of_order {
            self.frames_skipped_total.inc();
            self.frames_received_total.inc();
            per_stream_metrics.frames_received_total.inc();
            debug!(
                "Dropping OOO frame: ts={} (stream_id={})",
                frame.presentation_time, stream_id
            );
            return;
        }

        let buffer = {
            let mut buffers = self.buffers.write().unwrap();
            Arc::clone(buffers.entry(stream_id.clone()).or_insert_with(|| {
                info!("Creating new buffer for stream_id: {}", stream_id);
                Arc::new(RwLock::new(CircularBuffer::new()))
            }))
        };
        {
            let mut b = buffer.write().unwrap();
            if b.is_full() {
                // The first frame will be dropped by this circular buffer
                self.frames_skipped_total.inc();
            }
            b.push_back(frame);
        }
        self.frames_received_total.inc();
        per_stream_metrics.frames_received_total.inc();
    }

    pub fn set_send_to_receive_time_diff_per_stream(&self, stream_id: &str, diff: u64) {
        let per_stream_metrics = self.get_or_create_per_stream_metrics(stream_id);
        per_stream_metrics
            .send_to_receive_time_diff
            .set(diff as i64);
    }

    pub fn get_stream_ids(&self) -> Vec<String> {
        let buffers = self.buffers.read().unwrap();
        let mut stream_ids: Vec<String> = buffers.keys().cloned().collect();
        stream_ids.sort();
        stream_ids
    }

    pub fn get_frame_count(&self, stream_id: &String) -> usize {
        let buffers = self.buffers.read().unwrap();
        if let Some(buffer) = buffers.get(stream_id) {
            buffer.read().unwrap().len()
        } else {
            0
        }
    }

    pub fn get_highest_frame_count(&self) -> usize {
        let buffers = self.buffers.read().unwrap();
        buffers
            .values()
            .map(|buffer| buffer.read().unwrap().len())
            .max()
            .unwrap_or(0)
    }

    /// Remove up to `count` oldest frames from the buffer for `stream_id`.
    /// Returns the number of frames actually removed.
    pub fn remove_oldest_frames(&self, stream_id: &str, count: usize) -> usize {
        // Clone Arc so we can lock it outside the read-guard
        let buffer = {
            let buffers = self.buffers.read().unwrap();
            buffers.get(stream_id).cloned()
        };

        if let Some(buffer) = buffer {
            let mut buffer = buffer.write().unwrap();
            let mut removed = 0;
            for _ in 0..count {
                if buffer.is_empty() {
                    break;
                }
                buffer.pop_front();
                removed += 1;
                self.frames_skipped_total.inc();
            }
            removed
        } else {
            0
        }
    }

    /// Consume the "best" frame (closest in time to 'now') from the given stream,
    /// optionally removing older frames if the buffer is too big.
    pub fn consume_frame(&self, stream_id: &String) -> Option<FrameData> {
        if self.is_stream_deactivated(stream_id) {
            return None;
        }

        let buffer = {
            let buffers = self.buffers.read().unwrap();
            buffers.get(stream_id).cloned()
        };

        let Some(buffer) = buffer else {
            return None;
        };

        let current_time = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_micros() as u64,
            Err(_) => return None,
        };
        let five_seconds_ago = current_time.saturating_sub(5_000_000);

        let (consumed_frame, skipped_frames, catch_up_skips) = {
            let mut buffer = buffer.write().unwrap();
            if buffer.is_empty() {
                return None;
            }

            let mut skipped_frames = 0usize;
            let mut catch_up_skips = 0usize;

            // If the buffer is bigger than 2, remove frames older than 5 seconds
            // (we can tweak these numbers as needed)
            if buffer.len() > 2 {
                while buffer.len() > 1 {
                    let is_too_old = buffer.front().is_some_and(|front_frame| {
                        front_frame.presentation_time < five_seconds_ago
                    });
                    if !is_too_old {
                        break;
                    }

                    info!("Removing frame older than 5s for stream_id = {}", stream_id);
                    buffer.pop_front();
                    skipped_frames += 1;
                }

                if buffer.is_empty() {
                    return None;
                }
            }

            // We want the frame with presentation_time *closest* to now.
            // We'll do the same logic as before.
            if buffer.len() > 1 {
                let mut smallest_diff: u64 = u64::MAX;
                let mut frame_index: usize = 0;

                for (current_index, frame) in buffer.iter().enumerate() {
                    let diff =
                        (frame.presentation_time as i64 - current_time as i64).unsigned_abs();
                    if diff < smallest_diff {
                        smallest_diff = diff;
                        frame_index = current_index;
                    }
                }

                // Pop front until the closest frame is at the front
                // This is a simple catch-up strategy
                if frame_index > 0 {
                    for _ in 0..frame_index {
                        buffer.pop_front();
                    }
                    skipped_frames += frame_index;
                    catch_up_skips = frame_index;
                }
            }

            (buffer.pop_front(), skipped_frames, catch_up_skips)
        };

        if skipped_frames > 0 {
            self.frames_skipped_total.add(skipped_frames as i64);
        }
        if catch_up_skips > 0 {
            debug!(
                "Skipped {} frames for stream_id = {} (catch-up).",
                catch_up_skips, stream_id
            );
        }

        self.frames_consumed_total.inc();

        // Calculate and update our new metrics using the consumed frame
        if let Some(ref frame) = consumed_frame {
            if self.is_stream_deactivated(stream_id) {
                return None;
            }

            self.record_freeze_if_needed(stream_id, current_time, frame.presentation_time);
            self.record_completed_join(stream_id, current_time);

            let per_stream_metrics = self.get_or_create_per_stream_metrics(stream_id);

            let send_to_consume = current_time.saturating_sub(frame.send_time);
            let receive_to_consume = current_time.saturating_sub(frame.receive_time);

            self.send_to_consume_time_diff.set(send_to_consume as i64);
            self.receive_to_consume_time_diff
                .set(receive_to_consume as i64);
            self.point_count_metric.set(frame.point_count as i64);
            per_stream_metrics.frames_consumed_total.inc();
            per_stream_metrics
                .send_to_consume_time_diff
                .set(send_to_consume as i64);
            per_stream_metrics
                .receive_to_consume_time_diff
                .set(receive_to_consume as i64);
            per_stream_metrics
                .rendered_point_count
                .set(frame.point_count as i64);

            if let Some(quality_index) = frame.quality_index {
                self.record_rendered_quality(quality_index, current_time);
            }

            let total_point_count = {
                let mut last_consumed_point_counts =
                    self.last_consumed_point_counts.write().unwrap();
                last_consumed_point_counts.insert(stream_id.clone(), frame.point_count);
                last_consumed_point_counts.values().sum::<u64>()
            };
            self.total_point_count.set(total_point_count as i64);
        }

        // Finally, return the consumed frame
        consumed_frame
    }

    /// Calculates the total concurrent point count across all streams,
    /// using the last frame of each buffer.
    pub fn get_total_point_count(&self) -> u64 {
        let map = self.last_consumed_point_counts.read().unwrap();
        map.values().sum()
    }

    fn is_stream_deactivated(&self, stream_id: &str) -> bool {
        self.deactivated_streams.read().unwrap().contains(stream_id)
    }

    fn begin_join_tracking(&self, transport: ReceiverTransport, stream_id: &str) {
        if self.active_joined_streams.contains_key(stream_id)
            || self.pending_stream_joins.contains_key(stream_id)
        {
            return;
        }

        let Some(started_at_us) = current_time_us() else {
            return;
        };

        let is_rejoin = self.known_joined_streams.contains_key(stream_id);
        self.pending_stream_joins.insert(
            stream_id.to_string(),
            PendingJoinState {
                transport,
                started_at_us,
                is_rejoin,
            },
        );
    }

    fn record_completed_join(&self, stream_id: &str, current_time_us: u64) {
        let Some((stream_key, pending_join)) = self.pending_stream_joins.remove(stream_id) else {
            return;
        };

        let join_latency_us = current_time_us.saturating_sub(pending_join.started_at_us);
        let join_latency_gauge = join_latency_us.min(i64::MAX as u64) as i64;
        let metrics = self.lifecycle_metrics.for_transport(pending_join.transport);

        if pending_join.is_rejoin {
            metrics.rejoin_events_total.inc();
        } else {
            metrics.join_events_total.inc();
        }

        metrics.last_join_latency_us.set(join_latency_gauge);
        metrics.join_latency_sum_us.add(join_latency_gauge);
        metrics.join_latency_samples_total.inc();

        self.known_joined_streams
            .insert(stream_key.clone(), pending_join.transport);
        self.active_joined_streams
            .insert(stream_key, pending_join.transport);
    }

    fn record_freeze_if_needed(
        &self,
        stream_id: &str,
        current_consume_time_us: u64,
        current_presentation_time_us: u64,
    ) {
        let freeze_duration_us = if let Some(mut freeze_state) =
            self.per_stream_freeze_state.get_mut(stream_id)
        {
            let (next_state, freeze_duration_us) = observe_freeze_state(
                Some(*freeze_state),
                current_consume_time_us,
                current_presentation_time_us,
            );
            *freeze_state = next_state;
            freeze_duration_us
        } else {
            let (next_state, freeze_duration_us) =
                observe_freeze_state(None, current_consume_time_us, current_presentation_time_us);
            self.per_stream_freeze_state
                .insert(stream_id.to_string(), next_state);
            freeze_duration_us
        };

        if let Some(freeze_duration_us) = freeze_duration_us {
            self.freeze_metrics.record_freeze(freeze_duration_us);
        }
    }

    fn get_or_create_per_stream_metrics(&self, stream_id: &str) -> PerStreamMetrics {
        if let Some(metrics) = self.per_stream_metrics.get(stream_id) {
            return metrics.clone();
        }

        match self.per_stream_metrics.entry(stream_id.to_string()) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let metrics = PerStreamMetrics::new(&self.metrics, stream_id);
                entry.insert(metrics.clone());
                metrics
            }
        }
    }

    fn get_or_create_per_quality_render_metrics(&self, quality: u32) -> PerQualityRenderMetrics {
        if let Some(metrics) = self.per_quality_render_metrics.get(&quality) {
            return metrics.clone();
        }

        match self.per_quality_render_metrics.entry(quality) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let metrics = PerQualityRenderMetrics::new(&self.metrics, quality);
                entry.insert(metrics.clone());
                metrics
            }
        }
    }

    fn record_rendered_quality(&self, quality: u32, current_time: u64) {
        let previous_state = {
            let mut rendered_qoe_state = self.rendered_qoe_state.lock().unwrap();
            let previous_state = *rendered_qoe_state;
            *rendered_qoe_state = Some(RenderedQoEState {
                last_consume_time_us: current_time,
                last_quality: quality,
            });
            previous_state
        };

        if let Some(previous_state) = previous_state {
            let delta_us = current_time.saturating_sub(previous_state.last_consume_time_us);
            let previous_metrics =
                self.get_or_create_per_quality_render_metrics(previous_state.last_quality);
            previous_metrics.rendered_time_us_total.add(delta_us as i64);
            self.rendered_quality_switch_metrics
                .record_switch(previous_state.last_quality, quality);
        }

        let current_metrics = self.get_or_create_per_quality_render_metrics(quality);
        current_metrics.rendered_frames_total.inc();
        self.rendered_quality_tier.set(quality as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_freeze_state_bootstraps_expected_interval_without_freeze() {
        let (first_state, first_freeze_duration_us) =
            observe_freeze_state(None, 100_000, 1_000_000);
        assert_eq!(first_freeze_duration_us, None);
        assert_eq!(first_state.expected_frame_interval_us, None);

        let (second_state, second_freeze_duration_us) =
            observe_freeze_state(Some(first_state), 133_000, 1_033_000);
        assert_eq!(second_freeze_duration_us, None);
        assert_eq!(second_state.expected_frame_interval_us, Some(33_000));
    }

    #[test]
    fn observe_freeze_state_records_completed_freeze_from_consume_gap() {
        let previous_state = StreamFreezeState {
            last_consume_time_us: 133_000,
            last_presentation_time_us: 1_033_000,
            expected_frame_interval_us: Some(33_000),
        };

        let (next_state, freeze_duration_us) =
            observe_freeze_state(Some(previous_state), 250_000, 1_066_000);

        assert_eq!(freeze_duration_us, Some(84_000));
        assert_eq!(next_state.expected_frame_interval_us, Some(33_000));
    }

    #[test]
    fn update_expected_frame_interval_ignores_large_outlier_gap() {
        let expected_frame_interval_us =
            update_expected_frame_interval_us(Some(33_000), Some(120_000));
        assert_eq!(expected_frame_interval_us, Some(33_000));
    }
}

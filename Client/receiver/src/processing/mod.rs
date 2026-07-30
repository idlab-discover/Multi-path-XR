use crate::clock::{
    ClockDomain, ClockOffsetEstimator, ClockOffsetSample, ClockSampleTrust, ClockSourceKey,
};
use crate::processing::decoders::decode_data;
use crate::storage::{ReceiverTransport, Storage};
use pcf::{
    frame::PcfHeader,
    types::{RenderPrimitive as PcfRenderPrimitive, PCF_MAGIC},
};
use rayon::{ThreadPool, ThreadPoolBuilder};
use shared_utils::types::{
    FrameData, FramePayloadContainer, FramePayloadMetadata, FrameRenderPrimitive,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::runtime::{Builder, Runtime};
use tracing::{debug, error, info};

pub mod decoders;

const MAX_IN_FLIGHT_DECODES_PER_STREAM: usize = 2;

struct DecodeJob {
    stream_id: String,
    quality: u64,
    send_time: u64,
    presentation_time: u64,
    payload_metadata: FramePayloadMetadata,
    data: Vec<u8>,
}

impl DecodeJob {
    fn ordering_key(&self) -> (u64, u64) {
        (self.presentation_time, self.send_time)
    }
}

#[derive(Default)]
struct StreamDecodeState {
    in_flight: usize,
    pending: VecDeque<DecodeJob>,
    latest_dispatched: Option<(u64, u64)>,
}

struct DecodeScheduler {
    streams: HashMap<String, StreamDecodeState>,
    ready_streams: VecDeque<String>,
    in_flight: usize,
    pending: usize,
    max_in_flight: usize,
    max_in_flight_per_stream: usize,
}

struct DecodeSchedule {
    jobs: Vec<DecodeJob>,
    dropped: usize,
    in_flight: usize,
    pending: usize,
}

impl DecodeScheduler {
    fn new(max_in_flight: usize, max_in_flight_per_stream: usize) -> Self {
        Self {
            streams: HashMap::new(),
            ready_streams: VecDeque::new(),
            in_flight: 0,
            pending: 0,
            max_in_flight: max_in_flight.max(1),
            max_in_flight_per_stream: max_in_flight_per_stream.max(1),
        }
    }

    fn enqueue(&mut self, job: DecodeJob) -> DecodeSchedule {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        self.enqueue_at(job, now_us)
    }

    fn enqueue_at(&mut self, job: DecodeJob, now_us: u64) -> DecodeSchedule {
        let stream_id = job.stream_id.clone();
        let state = self.streams.entry(stream_id.clone()).or_default();
        let mut dropped = 0;
        let ordering_key = job.ordering_key();

        let is_older_than_dispatched = state
            .latest_dispatched
            .is_some_and(|latest| ordering_key <= latest);
        let is_duplicate_pending = state
            .pending
            .iter()
            .any(|pending| ordering_key == pending.ordering_key());

        if is_older_than_dispatched || is_duplicate_pending {
            dropped += 1;
        } else {
            let was_empty = state.pending.is_empty();
            let insertion_index = state
                .pending
                .iter()
                .position(|pending| ordering_key < pending.ordering_key())
                .unwrap_or(state.pending.len());
            state.pending.insert(insertion_index, job);
            self.pending += 1;
            if was_empty {
                self.ready_streams.push_back(stream_id);
            }
        }

        dropped += self.coalesce_due_frames(now_us);
        let jobs = self.take_ready_jobs();
        self.schedule(jobs, dropped)
    }

    fn complete(&mut self, stream_id: &str) -> DecodeSchedule {
        let now_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        self.complete_at(stream_id, now_us)
    }

    fn complete_at(&mut self, stream_id: &str, now_us: u64) -> DecodeSchedule {
        if let Some(state) = self.streams.get_mut(stream_id) {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
        self.in_flight = self.in_flight.saturating_sub(1);

        let dropped = self.coalesce_due_frames(now_us);
        let jobs = self.take_ready_jobs();
        self.schedule(jobs, dropped)
    }

    fn clear_pending(&mut self) -> DecodeSchedule {
        let dropped = self.pending;
        self.pending = 0;
        self.ready_streams.clear();
        self.streams.retain(|_, state| {
            state.pending.clear();
            state.in_flight > 0
        });
        self.schedule(Vec::new(), dropped)
    }

    fn remove_stream_pending(&mut self, stream_id: &str) -> DecodeSchedule {
        let dropped = self
            .streams
            .get_mut(stream_id)
            .map(|state| {
                let pending = state.pending.len();
                state.pending.clear();
                pending
            })
            .unwrap_or(0);
        self.pending = self.pending.saturating_sub(dropped);
        self.ready_streams.retain(|ready| ready != stream_id);

        if self
            .streams
            .get(stream_id)
            .is_some_and(|state| state.in_flight == 0)
        {
            self.streams.remove(stream_id);
        }

        self.schedule(Vec::new(), dropped)
    }

    fn coalesce_due_frames(&mut self, now_us: u64) -> usize {
        let mut dropped = 0;

        for state in self.streams.values_mut() {
            let due_count = state
                .pending
                .iter()
                .take_while(|job| job.presentation_time <= now_us)
                .count();
            if due_count > 1 {
                let drop_count = due_count - 1;
                state.pending.drain(..drop_count);
                dropped += drop_count;
            }
        }

        self.pending = self.pending.saturating_sub(dropped);
        dropped
    }

    fn take_ready_jobs(&mut self) -> Vec<DecodeJob> {
        let mut jobs = Vec::new();
        let mut blocked_streams = 0;

        while self.in_flight < self.max_in_flight && !self.ready_streams.is_empty() {
            if blocked_streams >= self.ready_streams.len() {
                break;
            }

            let stream_id = self.ready_streams.pop_front().unwrap();
            let Some(state) = self.streams.get_mut(&stream_id) else {
                continue;
            };

            if state.in_flight >= self.max_in_flight_per_stream {
                self.ready_streams.push_back(stream_id);
                blocked_streams += 1;
                continue;
            }

            let Some(job) = state.pending.pop_front() else {
                continue;
            };

            self.pending = self.pending.saturating_sub(1);
            self.in_flight += 1;
            state.in_flight += 1;
            state.latest_dispatched = Some(job.ordering_key());
            if !state.pending.is_empty() {
                self.ready_streams.push_back(stream_id);
            }
            jobs.push(job);
            blocked_streams = 0;
        }

        jobs
    }

    fn schedule(&self, jobs: Vec<DecodeJob>, dropped: usize) -> DecodeSchedule {
        DecodeSchedule {
            jobs,
            dropped,
            in_flight: self.in_flight,
            pending: self.pending,
        }
    }
}

pub struct ProcessingPipeline {
    storage: Arc<Storage>,
    thread_pool: Arc<ThreadPool>,
    decode_scheduler: Arc<Mutex<DecodeScheduler>>,
    pub runtime: Arc<Mutex<Option<Runtime>>>,
    disable_parser: bool,
    clock_offsets: Arc<ClockOffsetEstimator>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Once;
    use std::time::Duration;

    static METRICS_INIT: Once = Once::new();

    fn ensure_metrics_initialized() {
        if catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok() {
            return;
        }

        METRICS_INIT.call_once(|| {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                metrics::MetricsBuilder::new()
                    .add_label("mode", "processing-test")
                    .build()
            }));
        });

        assert!(catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok());
    }

    fn current_time_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }

    fn decode_job(stream_id: &str, presentation_time: u64) -> DecodeJob {
        DecodeJob {
            stream_id: stream_id.to_string(),
            quality: 0,
            send_time: presentation_time.saturating_sub(1),
            presentation_time,
            payload_metadata: FramePayloadMetadata::default(),
            data: vec![presentation_time as u8],
        }
    }

    #[test]
    fn decode_scheduler_preserves_all_frames_when_decode_keeps_up() {
        let mut scheduler = DecodeScheduler::new(4, 2);

        for presentation_time in 1..=5 {
            let schedule = scheduler.enqueue_at(decode_job("stream", presentation_time), 0);
            assert_eq!(schedule.dropped, 0);
            assert_eq!(schedule.jobs.len(), 1);
            assert_eq!(schedule.jobs[0].presentation_time, presentation_time);
            assert_eq!(schedule.pending, 0);

            let completed = scheduler.complete_at("stream", 0);
            assert!(completed.jobs.is_empty());
            assert_eq!(completed.in_flight, 0);
            assert_eq!(completed.pending, 0);
        }
    }

    #[test]
    fn decode_scheduler_keeps_only_newest_pending_frame_under_load() {
        let mut scheduler = DecodeScheduler::new(10, 2);

        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 10), 100)
                .jobs
                .len(),
            1
        );
        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 20), 100)
                .jobs
                .len(),
            1
        );

        let first_pending = scheduler.enqueue_at(decode_job("stream", 30), 100);
        assert!(first_pending.jobs.is_empty());
        assert_eq!(first_pending.pending, 1);
        assert_eq!(first_pending.dropped, 0);

        let replaced = scheduler.enqueue_at(decode_job("stream", 40), 100);
        assert!(replaced.jobs.is_empty());
        assert_eq!(replaced.pending, 1);
        assert_eq!(replaced.dropped, 1);

        let older_arrival = scheduler.enqueue_at(decode_job("stream", 35), 100);
        assert!(older_arrival.jobs.is_empty());
        assert_eq!(older_arrival.pending, 1);
        assert_eq!(older_arrival.dropped, 1);

        let completed = scheduler.complete_at("stream", 100);
        assert_eq!(completed.jobs.len(), 1);
        assert_eq!(completed.jobs[0].presentation_time, 40);
        assert_eq!(completed.in_flight, 2);
        assert_eq!(completed.pending, 0);
    }

    #[test]
    fn decode_scheduler_preserves_prefetched_future_frames() {
        let mut scheduler = DecodeScheduler::new(10, 2);

        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 10), 100)
                .jobs
                .len(),
            1
        );
        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 20), 100)
                .jobs
                .len(),
            1
        );

        let first_future = scheduler.enqueue_at(decode_job("stream", 300), 100);
        assert!(first_future.jobs.is_empty());
        assert_eq!(first_future.pending, 1);
        assert_eq!(first_future.dropped, 0);

        let second_future = scheduler.enqueue_at(decode_job("stream", 400), 100);
        assert!(second_future.jobs.is_empty());
        assert_eq!(second_future.pending, 2);
        assert_eq!(second_future.dropped, 0);

        let completed = scheduler.complete_at("stream", 100);
        assert_eq!(completed.jobs.len(), 1);
        assert_eq!(completed.jobs[0].presentation_time, 300);
        assert_eq!(completed.pending, 1);
        assert_eq!(completed.dropped, 0);
    }

    #[test]
    fn decode_scheduler_orders_out_of_order_prefetched_future_frames() {
        let mut scheduler = DecodeScheduler::new(10, 2);

        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 10), 100)
                .jobs
                .len(),
            1
        );
        assert_eq!(
            scheduler
                .enqueue_at(decode_job("stream", 20), 100)
                .jobs
                .len(),
            1
        );

        let later_future = scheduler.enqueue_at(decode_job("stream", 400), 100);
        assert_eq!(later_future.pending, 1);
        assert_eq!(later_future.dropped, 0);

        let earlier_future = scheduler.enqueue_at(decode_job("stream", 300), 100);
        assert_eq!(earlier_future.pending, 2);
        assert_eq!(earlier_future.dropped, 0);

        let completed = scheduler.complete_at("stream", 100);
        assert_eq!(completed.jobs.len(), 1);
        assert_eq!(completed.jobs[0].presentation_time, 300);
        assert_eq!(completed.pending, 1);
    }

    #[test]
    fn decode_scheduler_rejects_a_late_arrival_older_than_dispatched_work() {
        let mut scheduler = DecodeScheduler::new(1, 1);

        let first = scheduler.enqueue_at(decode_job("stream", 20), 100);
        assert_eq!(first.jobs.len(), 1);
        assert_eq!(scheduler.complete_at("stream", 100).in_flight, 0);

        let stale = scheduler.enqueue_at(decode_job("stream", 10), 100);
        assert!(stale.jobs.is_empty());
        assert_eq!(stale.dropped, 1);
        assert_eq!(stale.pending, 0);
    }

    #[test]
    fn decode_scheduler_preserves_pending_work_for_other_streams() {
        let mut scheduler = DecodeScheduler::new(2, 2);

        assert_eq!(scheduler.enqueue_at(decode_job("a", 10), 0).jobs.len(), 1);
        assert_eq!(scheduler.enqueue_at(decode_job("b", 10), 0).jobs.len(), 1);
        assert!(scheduler.enqueue_at(decode_job("a", 20), 0).jobs.is_empty());
        assert!(scheduler.enqueue_at(decode_job("b", 20), 0).jobs.is_empty());

        let after_a = scheduler.complete_at("a", 0);
        assert_eq!(after_a.jobs.len(), 1);
        assert_eq!(after_a.jobs[0].stream_id, "a");

        let after_b = scheduler.complete_at("b", 0);
        assert_eq!(after_b.jobs.len(), 1);
        assert_eq!(after_b.jobs[0].stream_id, "b");
    }

    #[test]
    fn ingest_data_for_transport_corrects_frame_timestamps_before_storage() {
        ensure_metrics_initialized();

        let storage = Arc::new(Storage::new());
        let pipeline = ProcessingPipeline::new(storage.clone(), 1, true);
        let clock_source = ClockSourceKey::with_server_id(ClockDomain::Dash, "server-a");
        pipeline.observe_clock_offset_us_for_source(
            clock_source.clone(),
            ClockSampleTrust::HighRtt,
            50_000,
        );

        let stream_id = "clock_corrected_stream".to_string();
        storage.activate_stream(&stream_id);
        let now_us = current_time_us();
        let raw_send_time_us = now_us + 40_000;
        let raw_presentation_time_us = now_us + 50_000;

        pipeline.ingest_data_for_source(
            clock_source,
            stream_id.clone(),
            1,
            raw_send_time_us,
            raw_presentation_time_us,
            vec![1, 2, 3],
        );

        std::thread::sleep(Duration::from_millis(50));
        let frame = storage
            .consume_frame(&stream_id)
            .expect("corrected frame should be consumable");

        assert_eq!(frame.send_time, raw_send_time_us - 50_000);
        assert_eq!(frame.presentation_time, raw_presentation_time_us - 50_000);
    }
}
crate::log_drop!(ProcessingPipeline);

impl ProcessingPipeline {
    pub fn new(storage: Arc<Storage>, thread_count: usize, disable_parser: bool) -> Self {
        // Initialize thread pool
        info!("Creating processing pipeline");

        let thread_pool = Arc::new(
            ThreadPoolBuilder::new()
                .thread_name(|i| format!("PP_TP w-{}", i + 1))
                .num_threads(thread_count)
                .build()
                .expect("Failed to build thread pool"),
        );
        let decode_scheduler = Arc::new(Mutex::new(DecodeScheduler::new(
            thread_count,
            MAX_IN_FLIGHT_DECODES_PER_STREAM,
        )));
        let runtime = Arc::new(Mutex::new(Some(
            Builder::new_multi_thread()
                .thread_name_fn(|| {
                    static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize =
                        std::sync::atomic::AtomicUsize::new(0);
                    let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    format!("PP_RU w-{id}")
                })
                .enable_all()
                .build()
                .expect("Failed to build runtime"),
        )));

        Self {
            storage,
            thread_pool,
            decode_scheduler,
            runtime,
            disable_parser,
            clock_offsets: ClockOffsetEstimator::new(),
        }
    }

    pub fn stop(&self) {
        let schedule = self.decode_scheduler.lock().unwrap().clear_pending();
        self.record_decode_schedule(&schedule);

        if let Some(rt) = self.runtime.lock().unwrap().take() {
            rt.shutdown_timeout(std::time::Duration::from_secs(1));
            info!("Processing pipeline runtime stopped");
        } else {
            error!("Processing pipeline runtime was already stopped or not initialized");
        }
    }

    pub fn empty_frame(&self, stream_id: String) {
        self.storage.empty_frame(stream_id.clone());
        self.storage
            .set_send_to_receive_time_diff_per_stream(&stream_id, 0);
    }

    pub fn activate_stream(&self, stream_id: String) {
        self.storage.activate_stream(&stream_id);
    }

    pub fn activate_stream_for_transport(&self, transport: ReceiverTransport, stream_id: String) {
        self.storage
            .activate_stream_for_transport(transport, &stream_id);
    }

    pub fn remove_stream(&self, stream_id: String) {
        let schedule = self
            .decode_scheduler
            .lock()
            .unwrap()
            .remove_stream_pending(&stream_id);
        self.record_decode_schedule(&schedule);
        self.storage.remove_stream(&stream_id);
        self.storage
            .set_send_to_receive_time_diff_per_stream(&stream_id, 0);
    }

    pub fn observe_clock_offset_us(&self, domain: ClockDomain, offset_us: i64) -> Option<i64> {
        self.clock_offsets.observe_offset_us(
            ClockSourceKey::for_transport(domain),
            ClockSampleTrust::HighRtt,
            offset_us,
        )
    }

    pub fn observe_clock_offset_sample(
        &self,
        source: ClockSourceKey,
        trust: ClockSampleTrust,
        sample: ClockOffsetSample,
    ) -> Option<i64> {
        self.clock_offsets.observe_sample(source, trust, sample)
    }

    pub fn observe_clock_offset_us_for_source(
        &self,
        source: ClockSourceKey,
        trust: ClockSampleTrust,
        offset_us: i64,
    ) -> Option<i64> {
        self.clock_offsets
            .observe_offset_us(source, trust, offset_us)
    }

    pub fn ingest_data(
        &self,
        stream_id: String,
        quality: u64,
        send_time: u64,
        presentation_time: u64,
        data: Vec<u8>,
    ) {
        self.ingest_data_for_transport(
            ClockDomain::Unknown,
            stream_id,
            quality,
            send_time,
            presentation_time,
            data,
        );
    }

    pub fn ingest_data_for_transport(
        &self,
        clock_domain: ClockDomain,
        stream_id: String,
        quality: u64,
        send_time: u64,
        presentation_time: u64,
        data: Vec<u8>,
    ) {
        self.ingest_data_for_source(
            ClockSourceKey::for_transport(clock_domain),
            stream_id,
            quality,
            send_time,
            presentation_time,
            data,
        );
    }

    pub fn ingest_data_for_source(
        &self,
        clock_source: ClockSourceKey,
        stream_id: String,
        quality: u64,
        send_time: u64,
        presentation_time: u64,
        data: Vec<u8>,
    ) {
        self.ingest_frame_data_for_source(
            clock_source,
            stream_id,
            quality,
            send_time,
            presentation_time,
            FramePayloadMetadata::default(),
            data,
        );
    }

    pub fn ingest_frame_data_for_source(
        &self,
        clock_source: ClockSourceKey,
        mut stream_id: String,
        mut quality: u64,
        mut send_time: u64,
        mut presentation_time: u64,
        mut payload_metadata: FramePayloadMetadata,
        data: Vec<u8>,
    ) {
        normalize_direct_pcf_frame(
            &mut stream_id,
            &mut quality,
            &mut send_time,
            &mut presentation_time,
            &mut payload_metadata,
            &data,
        );

        let storage = self.storage.clone();
        let thread_pool = self.thread_pool.clone();
        let disable_parser = self.disable_parser;
        let timestamp_correction =
            self.clock_offsets
                .correct_frame_timestamps(clock_source, send_time, presentation_time);

        storage.quality_metric.set(quality as i64);

        let job = DecodeJob {
            stream_id,
            quality,
            send_time: timestamp_correction.send_time_us,
            presentation_time: timestamp_correction.presentation_time_us,
            payload_metadata,
            data,
        };
        let schedule = self.decode_scheduler.lock().unwrap().enqueue(job);
        self.record_decode_schedule(&schedule);
        Self::spawn_decode_jobs(
            schedule.jobs,
            storage,
            thread_pool,
            self.decode_scheduler.clone(),
            disable_parser,
        );
    }

    fn spawn_decode_jobs(
        jobs: Vec<DecodeJob>,
        storage: Arc<Storage>,
        thread_pool: Arc<ThreadPool>,
        decode_scheduler: Arc<Mutex<DecodeScheduler>>,
        disable_parser: bool,
    ) {
        for job in jobs {
            let storage = storage.clone();
            let thread_pool_for_completion = thread_pool.clone();
            let scheduler = decode_scheduler.clone();
            let spawn_pool = thread_pool.clone();

            spawn_pool.spawn(move || {
                let stream_id = job.stream_id.clone();
                Self::decode_job(job, storage.clone(), disable_parser);

                let schedule = scheduler.lock().unwrap().complete(&stream_id);
                Self::record_decode_schedule_for_storage(&storage, &schedule);
                Self::spawn_decode_jobs(
                    schedule.jobs,
                    storage,
                    thread_pool_for_completion,
                    scheduler,
                    disable_parser,
                );
            });
        }
    }

    fn decode_job(job: DecodeJob, storage: Arc<Storage>, disable_parser: bool) {
        let start_time = SystemTime::now();
        let frame_data = if disable_parser {
            Ok(FrameData {
                send_time: job.send_time,
                presentation_time: job.presentation_time,
                receive_time: 0,
                quality_index: u32::try_from(job.quality).ok(),
                render_primitive: FrameRenderPrimitive::Points,
                error_count: 0,
                point_count: 1,
                coordinates: vec![0.0, 0.0, 0.0],
                colors: vec![255, 255, 255],
                gaussian_scales: Vec::new(),
                gaussian_rotations: Vec::new(),
            })
        } else {
            decode_data(
                job.send_time,
                job.presentation_time,
                job.payload_metadata,
                &job.data,
            )
        };

        match frame_data {
            Ok(mut frame_data) => {
                frame_data.quality_index = u32::try_from(job.quality).ok();
                if frame_data.error_count > 0 {
                    error!(
                        "Frame data has errors (stream_id: {}, error_count: {})",
                        job.stream_id, frame_data.error_count
                    );
                }
                if frame_data.point_count == 0 {
                    debug!("Frame data has no points (stream_id: {})", job.stream_id);
                    return;
                }

                let decode_duration = match SystemTime::now().duration_since(start_time) {
                    Ok(duration) => duration.as_micros() as u64,
                    Err(e) => {
                        error!("Failed to calculate decode duration: {:?}", e);
                        return;
                    }
                };
                storage.decode_time.set(decode_duration as i64);

                frame_data.receive_time =
                    start_time.duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
                let send_to_receive = frame_data.receive_time.saturating_sub(frame_data.send_time);
                storage
                    .send_to_receive_time_diff
                    .set(send_to_receive as i64);
                storage.set_send_to_receive_time_diff_per_stream(&job.stream_id, send_to_receive);
                storage.insert_frame(job.stream_id, frame_data);
            }
            Err(e) => {
                error!("Failed to decode frame data: {:?}", e);
            }
        }
    }

    fn record_decode_schedule(&self, schedule: &DecodeSchedule) {
        Self::record_decode_schedule_for_storage(&self.storage, schedule);
    }

    fn record_decode_schedule_for_storage(storage: &Storage, schedule: &DecodeSchedule) {
        if schedule.dropped > 0 {
            storage
                .frames_dropped_before_decode_total
                .add(schedule.dropped as i64);
        }
        storage
            .predecode_frames_in_flight
            .set(schedule.in_flight as i64);
        storage
            .predecode_frames_pending
            .set(schedule.pending as i64);
    }
}

fn normalize_direct_pcf_frame(
    stream_id: &mut String,
    quality: &mut u64,
    send_time: &mut u64,
    presentation_time: &mut u64,
    payload_metadata: &mut FramePayloadMetadata,
    data: &[u8],
) {
    if !data.starts_with(PCF_MAGIC) {
        return;
    }

    let Ok(header) = PcfHeader::parse(data) else {
        return;
    };

    payload_metadata.container = FramePayloadContainer::Pcf;
    if let Some(render_primitive) = header.render_primitive {
        payload_metadata.primitive = match render_primitive {
            PcfRenderPrimitive::Points => FrameRenderPrimitive::Points,
            PcfRenderPrimitive::GaussianSplats => FrameRenderPrimitive::GaussianSplats,
        };
    }
    if let Some(header_send_time) = header.send_time_us {
        *send_time = header_send_time;
    }
    if let Some(header_presentation_time) = header.presentation_time_us {
        *presentation_time = header_presentation_time;
    }
    if let Some(quality_index) = header.quality_index {
        *quality = u64::from(quality_index);
    }
    if let Some(client_id) = header.client_id {
        *stream_id = format!("client_{}_{}", client_id, *quality);
    }
}

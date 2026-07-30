use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::processing::aggregator::SpatialFrameAggregator;
use crate::processing::ProcessingPipeline;
use crate::timing::{
    frame_index_for_elapsed, frame_offset_duration, frame_period_duration,
    scheduler_lateness_gauge, sleep_until_and_record_lateness,
};
use circular_buffer::CircularBuffer;
use metrics::get_metrics;
use pcf::{
    frame::{PcfFrameMeta, PcfHeader},
    types::RenderPrimitive as PcfRenderPrimitive,
};
use prometheus::IntGauge;
use shared_utils::types::{
    FramePayloadContainer, FramePayloadMetadata, FrameRenderPrimitive, FrameTaskData,
    SpatialFrameData,
};
use spatial_codecs::encoder::EncodingFormat;
//use rayon::ThreadPoolBuilder;
use tracing::{debug, error, instrument, warn};

#[derive(Clone, Debug)]
pub struct EgressCommonMetrics {
    pub pc_combination_time: IntGauge,
    pub pc_encoding_time: IntGauge,
    pub bytes_to_send: IntGauge,
    pub number_of_combined_frames: IntGauge,
    pub frame_drops_full_egress_buffer: IntGauge,
}

impl EgressCommonMetrics {
    pub fn new() -> Self {
        let metrics = get_metrics();
        let pc_combination_time = metrics
            .get_or_create_gauge(
                "pc_combination_time",
                "Time taken to generate a combined spatial frame",
            )
            .unwrap();

        let pc_encoding_time = metrics
            .get_or_create_gauge(
                "pc_encoding_time",
                "Time taken to encode a combined spatial frame",
            )
            .unwrap();

        let bytes_to_send = metrics
            .get_or_create_gauge("bytes_to_send", "Number of bytes to send")
            .unwrap();

        let number_of_combined_frames = metrics
            .get_or_create_gauge("number_of_combined_frames", "Number of combined frames generated and pushed to the egress buffer based on the frames in the aggregator")
            .unwrap();

        let frame_drops_full_egress_buffer = metrics
            .get_or_create_gauge(
                "frame_drops_full_egress_buffer",
                "Number of dropped frames due to a full egress buffer.",
            )
            .unwrap();

        Self {
            pc_combination_time,
            pc_encoding_time,
            bytes_to_send,
            number_of_combined_frames,
            frame_drops_full_egress_buffer,
        }
    }
}

#[derive(Debug)]
pub struct AtomicEncodingFormat {
    inner: AtomicU8,
}

impl AtomicEncodingFormat {
    pub fn new(format: EncodingFormat) -> Self {
        Self {
            inner: AtomicU8::new(Self::encode(format)),
        }
    }

    #[inline]
    pub fn load(&self) -> EncodingFormat {
        Self::decode(self.inner.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn store(&self, format: EncodingFormat) {
        self.inner.store(Self::encode(format), Ordering::Relaxed);
    }

    #[inline]
    const fn encode(format: EncodingFormat) -> u8 {
        match format {
            EncodingFormat::Ply => 0,
            EncodingFormat::Draco => 1,
            EncodingFormat::Gsplat16 => 2,
            EncodingFormat::LASzip => 3,
            EncodingFormat::Tmf => 4,
            EncodingFormat::Bitcode => 5,
            EncodingFormat::Gzip => 6,
            EncodingFormat::Zstd => 7,
            EncodingFormat::Lz4 => 8,
            EncodingFormat::Snappy => 9,
            EncodingFormat::Sogp => 10,
            EncodingFormat::Quantize => 11,
            EncodingFormat::Openzl => 12,
        }
    }

    #[inline]
    const fn decode(raw: u8) -> EncodingFormat {
        match raw {
            0 => EncodingFormat::Ply,
            1 => EncodingFormat::Draco,
            2 => EncodingFormat::Gsplat16,
            3 => EncodingFormat::LASzip,
            4 => EncodingFormat::Tmf,
            5 => EncodingFormat::Bitcode,
            6 => EncodingFormat::Gzip,
            7 => EncodingFormat::Zstd,
            8 => EncodingFormat::Lz4,
            9 => EncodingFormat::Snappy,
            10 => EncodingFormat::Sogp,
            11 => EncodingFormat::Quantize,
            12 => EncodingFormat::Openzl,
            _ => EncodingFormat::Draco,
        }
    }
}

/// Starts the generator thread that periodically generates combined spatial frames
/// and encodes them into frames.
#[instrument(skip_all, fields(egress_name = %egress_name))]
pub fn start_generator_thread(
    egress_name: String,
    processing_pipeline: Arc<ProcessingPipeline>,
    aggregator: Arc<SpatialFrameAggregator>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    fps: Arc<AtomicU32>,
    encoding_format: Arc<AtomicEncodingFormat>,
    max_number_of_primitives: Arc<AtomicU64>,
) {
    let processing_pipeline_clone = processing_pipeline.clone();
    let aggregator_clone = aggregator.clone();
    let frame_buffer_clone = frame_buffer.clone();
    let fps_clone = fps.clone();
    let encoding_format_clone = encoding_format.clone();
    let max_number_of_primitives_clone = max_number_of_primitives.clone();
    let egress_name_clone = egress_name.clone();
    let thread_name = format!("{egress_name_clone} Generator Thread");
    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        generate_and_send_combined_spatial_frames(
            egress_name_clone,
            processing_pipeline_clone,
            aggregator_clone,
            frame_buffer_clone,
            fps_clone,
            encoding_format_clone,
            max_number_of_primitives_clone,
        );
    });
}

/// Periodically generates combined spatial frames and encodes them into frames.
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn generate_and_send_combined_spatial_frames(
    egress_name: String,
    processing_pipeline: Arc<ProcessingPipeline>,
    aggregator: Arc<SpatialFrameAggregator>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    fps: Arc<AtomicU32>,
    encoding_format: Arc<AtomicEncodingFormat>,
    max_number_of_primitives: Arc<AtomicU64>,
) {
    let current_in_queue = Arc::new(AtomicI32::new(0));
    let egress_common_metrics = EgressCommonMetrics::new();
    let pc_combination_time = egress_common_metrics.pc_combination_time;
    let pc_encoding_time = egress_common_metrics.pc_encoding_time;
    let bytes_to_send = egress_common_metrics.bytes_to_send;
    let number_of_combined_frames = egress_common_metrics.number_of_combined_frames;
    let frame_drops_full_egress_buffer = egress_common_metrics.frame_drops_full_egress_buffer;

    //// Initialize thread pool
    let thread_count = processing_pipeline.thread_pool.current_num_threads();
    /*
    let thread_pool = Arc::new(
        ThreadPoolBuilder::new()
            .thread_name(|i| format!("E_TP w-{}", i+1))
            .num_threads(thread_count)
            .build()
            .expect("Failed to build thread pool"),
    );
    */

    let scheduler_lateness = scheduler_lateness_gauge(&format!("egress:{egress_name}"));
    let mut schedule_start = Instant::now();
    let mut next_frame_index = 0_u64;
    let mut active_fps = fps.load(Ordering::Relaxed).max(1);

    loop {
        let fps_value = fps.load(Ordering::Relaxed).max(1);
        if fps_value != active_fps {
            active_fps = fps_value;
            schedule_start = Instant::now();
            next_frame_index = 0;
        }
        let frame_duration = frame_period_duration(fps_value);

        let now = Instant::now();
        let mut frame_target = schedule_start + frame_offset_duration(next_frame_index, fps_value);
        if now >= frame_target && now.duration_since(frame_target) >= frame_duration {
            let elapsed = now.duration_since(schedule_start);
            let current_grid_index = frame_index_for_elapsed(elapsed, fps_value);
            if current_grid_index > next_frame_index {
                next_frame_index = current_grid_index;
                frame_target = schedule_start + frame_offset_duration(next_frame_index, fps_value);
            }
        }

        sleep_until_and_record_lateness(frame_target, &scheduler_lateness);
        next_frame_index = next_frame_index.saturating_add(1);
        let start_time = Instant::now();

        // There may not be more then 500 ms of frames in the queue
        // First, calculate the max number of frames that can be in the queue
        let max_frame_delay = 500; // ms
        let max_frame_count_in_queue =
            ((max_frame_delay / (1000 / fps_value)) as i32).min(thread_count.try_into().unwrap());
        // Then, check if the current in queue count is greater than the max frame count in queue
        let current_in_queue_clone = current_in_queue.load(Ordering::Relaxed);
        if current_in_queue_clone > max_frame_count_in_queue {
            warn!("Frame generation is too slow, skipping frame generation. There are {} frames in the queue.", current_in_queue_clone);
            continue;
        }

        // Generate the spatial frame for the egress
        let generate_start_time = start_time;
        handle_spatial_frame_generation(
            &egress_name,
            &processing_pipeline,
            &aggregator,
            &frame_buffer,
            &encoding_format,
            &max_number_of_primitives,
            &current_in_queue,
            &pc_combination_time,
            &pc_encoding_time,
            &bytes_to_send,
            &number_of_combined_frames,
            &frame_drops_full_egress_buffer,
            generate_start_time,
            false, // to do; add ring buffer bypass
        );

        let processing_time = start_time.elapsed();

        if processing_time > frame_duration {
            warn!(
                "Processing time exceeded frame duration by {:?}",
                processing_time - frame_duration
            );
        }
    }
}

/// Handles frame generation and encoding.
#[allow(clippy::too_many_arguments)]
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn handle_spatial_frame_generation(
    egress_name: &str,
    processing_pipeline: &Arc<ProcessingPipeline>,
    aggregator: &Arc<SpatialFrameAggregator>,
    frame_buffer: &Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    encoding_format: &Arc<AtomicEncodingFormat>,
    max_number_of_primitives: &Arc<AtomicU64>,
    current_in_queue: &Arc<AtomicI32>,
    pc_combination_time: &IntGauge,
    pc_encoding_time: &IntGauge,
    bytes_to_send: &IntGauge,
    number_of_combined_frames: &IntGauge,
    frame_drops_full_egress_buffer: &IntGauge,
    generate_start_time: Instant,
    ring_buffer_bypass: bool,
) {
    // debug!("Handling combined spatial frame generation");

    current_in_queue.fetch_add(1, Ordering::Relaxed);

    // Generate the combined spatial frame
    let max_primitives = max_number_of_primitives.load(Ordering::Relaxed);
    let combined_spatial_frame = aggregator.generate_combined_spatial_frame(max_primitives);

    pc_combination_time.set(generate_start_time.elapsed().as_micros() as i64);

    // If the combined spatial frame is empty, then skip
    if combined_spatial_frame.is_empty() {
        // debug!("Combined spatial frame is empty, skipping frame encoding");
        // Decrease the current in queue count
        current_in_queue.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    let thread_pool = processing_pipeline.thread_pool.clone();
    let egress_name = egress_name.to_string();
    let processing_pipeline = Arc::clone(processing_pipeline);
    let frame_buffer = Arc::clone(frame_buffer);
    let encoding_format = Arc::clone(encoding_format);
    let current_in_queue = Arc::clone(current_in_queue);
    let pc_encoding_time = pc_encoding_time.clone();
    let bytes_to_send = bytes_to_send.clone();
    let number_of_combined_frames = number_of_combined_frames.clone();
    let frame_drops_full_egress_buffer = frame_drops_full_egress_buffer.clone();
    thread_pool.spawn(move || {
        encode_spatial_frame(
            egress_name,
            combined_spatial_frame,
            processing_pipeline,
            frame_buffer,
            encoding_format,
            current_in_queue,
            pc_encoding_time,
            bytes_to_send,
            number_of_combined_frames,
            frame_drops_full_egress_buffer,
            ring_buffer_bypass,
        );
    });
}

// Encode the combined spatial frame
#[allow(clippy::too_many_arguments)]
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn encode_spatial_frame(
    egress_name: String,
    combined_spatial_frame: SpatialFrameData,
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    encoding_format: Arc<AtomicEncodingFormat>,
    current_in_queue: Arc<AtomicI32>,
    pc_encoding_time: IntGauge,
    bytes_to_send: IntGauge,
    number_of_combined_frames: IntGauge,
    frame_drops_full_egress_buffer: IntGauge,
    _ring_buffer_bypass: bool,
) {
    debug!("Encoding combined spatial frame");
    let encoding_start_time = Instant::now();

    let encoding_format = encoding_format.load();
    let encoded_spatial_frame = processing_pipeline.encode(combined_spatial_frame, encoding_format);
    match encoded_spatial_frame {
        Ok(encoded_data) => {
            push_encoded_frame_data(
                &egress_name,
                &frame_buffer,
                encoded_data,
                None,
                &bytes_to_send,
                &frame_drops_full_egress_buffer,
                &number_of_combined_frames,
            );
        }
        Err(e) => {
            // Handle encoding error
            error!("Encoding error: {:?}", e);
        }
    };

    pc_encoding_time.set(encoding_start_time.elapsed().as_micros() as i64);

    // Decrease the current in queue count
    current_in_queue.fetch_sub(1, Ordering::Relaxed);
}

/// Push a fully-formed FrameTaskData into the egress buffer (by default),
/// or bypass it if `ring_buffer_bypass` is true.
pub fn push_encoded_frame_data(
    egress_name: &str,
    frame_buffer: &Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    frame: FrameTaskData,
    ring_buffer_bypass: Option<Box<dyn Fn(FrameTaskData) + Send + 'static>>,
    bytes_to_send: &IntGauge,
    frame_drops_full_egress_buffer: &IntGauge,
    number_of_combined_frames: &IntGauge,
) {
    bytes_to_send.set(frame.data.len() as i64);

    if let Some(ref bypass_fn) = ring_buffer_bypass {
        // Bypass ring buffer => you could directly emit here (if you like)
        // For example:
        debug!(
            "({}) ring_buffer_bypass=TRUE, skipping the buffer and directly emitting frame",
            egress_name
        );
        // Call a direct “emit_frame_data” if you want immediate send
        bypass_fn(frame);
        return;
    }

    // Otherwise, push into the ring buffer as before:
    let mut buffer = frame_buffer.lock().unwrap();
    if buffer.is_full() {
        debug!(
            "({}) Frame buffer is full, dropping oldest frame",
            egress_name
        );
        frame_drops_full_egress_buffer.inc();
    }
    buffer.push_back(frame);
    number_of_combined_frames.inc();
    //debug!("({}) Pushed encoded frame to buffer", egress_name);
}

/// If we already have `Vec<u8>` representing the final frame payload
/// plus the creation & presentation timestamps, this function
/// wraps them into a `FrameTaskData` and pushes/bypasses the ring buffer.
#[instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
pub fn push_preencoded_frame_data(
    egress_name: &str,
    frame_buffer: &Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    creation_time: u64,
    presentation_time: u64,
    data: Vec<u8>,
    payload_metadata: Option<FramePayloadMetadata>,
    ring_buffer_bypass: Option<Box<dyn Fn(FrameTaskData) + Send + 'static>>,
    metrics: &EgressCommonMetrics,
    client_id: Option<u64>,
    quality_index: Option<u32>,
) {
    let payload_metadata = payload_metadata.unwrap_or_else(|| infer_payload_metadata(&data));

    // Build a new FrameTaskData using the provided times and data
    let frame = FrameTaskData {
        send_time: creation_time,
        presentation_time,
        payload_metadata,
        data, // Move the data into the struct
        client_id,
        quality_index,
    };

    // Reuse the same ring-buffer push function
    push_encoded_frame_data(
        egress_name,
        frame_buffer,
        frame,
        ring_buffer_bypass,
        &metrics.bytes_to_send,
        &metrics.frame_drops_full_egress_buffer,
        &metrics.number_of_combined_frames,
    );
}

fn infer_payload_metadata(data: &[u8]) -> FramePayloadMetadata {
    if let Ok(header) = PcfHeader::parse(data) {
        return FramePayloadMetadata {
            container: FramePayloadContainer::Pcf,
            primitive: header
                .render_primitive
                .map(|primitive| match primitive {
                    PcfRenderPrimitive::Points => FrameRenderPrimitive::Points,
                    PcfRenderPrimitive::GaussianSplats => FrameRenderPrimitive::GaussianSplats,
                })
                .unwrap_or_else(|| infer_payload_primitive(header.payload)),
        };
    }

    FramePayloadMetadata {
        container: FramePayloadContainer::Raw,
        primitive: infer_payload_primitive(data),
    }
}

fn infer_payload_primitive(data: &[u8]) -> FrameRenderPrimitive {
    if data.get(0..3) == Some(b"GSP") && data.get(4).is_some_and(|flags| (flags & 0b0000_0001) != 0)
    {
        FrameRenderPrimitive::GaussianSplats
    } else {
        FrameRenderPrimitive::Points
    }
}

pub fn frame_task_to_pcf_wire(
    frame: &FrameTaskData,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let render_primitive = match frame.payload_metadata.primitive {
        FrameRenderPrimitive::Points => PcfRenderPrimitive::Points,
        FrameRenderPrimitive::GaussianSplats => PcfRenderPrimitive::GaussianSplats,
    };
    let existing_header = PcfHeader::parse(&frame.data).ok();
    let payload = existing_header
        .as_ref()
        .map_or(frame.data.as_slice(), |header| header.payload);
    let meta = PcfFrameMeta {
        key: existing_header
            .as_ref()
            .is_none_or(|header| header.flags.contains(pcf::types::Flags::KEY)),
        delta: existing_header
            .as_ref()
            .is_some_and(|header| header.flags.contains(pcf::types::Flags::DELTA)),
        codec_magic: existing_header
            .as_ref()
            .and_then(|header| header.codec_magic),
        stream_id: existing_header.as_ref().and_then(|header| header.stream_id),
        seq: existing_header.as_ref().and_then(|header| header.seq),
        send_time_us: existing_header
            .as_ref()
            .and_then(|header| header.send_time_us)
            .or(Some(frame.send_time)),
        presentation_time_us: existing_header
            .as_ref()
            .and_then(|header| header.presentation_time_us)
            .or(Some(frame.presentation_time)),
        ref_seq: existing_header.as_ref().and_then(|header| header.ref_seq),
        client_id: existing_header
            .as_ref()
            .and_then(|header| header.client_id)
            .or(frame.client_id),
        quality_index: existing_header
            .as_ref()
            .and_then(|header| header.quality_index)
            .or(frame.quality_index),
        render_primitive: existing_header
            .as_ref()
            .and_then(|header| header.render_primitive)
            .or(Some(render_primitive)),
        ..Default::default()
    };

    let mut out = Vec::with_capacity(64 + payload.len());
    PcfHeader::write_frame_to(&mut out, &meta, payload)
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
    Ok(out)
}

/// Starts the transmission thread that sends frames to clients.
#[instrument(skip_all, fields(egress_name = %egress_name))]
pub fn start_transmission_thread<F>(
    egress_name: String,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    emit_frame_data: F,
    disable_frame_drops: bool,
) where
    F: Fn(FrameTaskData) + Send + 'static + Clone,
{
    let frame_buffer_clone = frame_buffer.clone();
    let emit_frame_data_clone = emit_frame_data.clone();
    let egress_name_clone = egress_name.clone();
    let thread_name = format!("{egress_name} Transmission Thread");

    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        send_frames_to_clients(
            egress_name_clone,
            frame_buffer_clone,
            emit_frame_data_clone,
            disable_frame_drops,
        );
    });
}

/// Sends frames to clients, handling frame timing and emission.
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn send_frames_to_clients<F>(
    egress_name: String,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    emit_frame_data: F,
    disable_frame_drops: bool,
) where
    F: Fn(FrameTaskData) + Send + 'static + Clone,
{
    let metrics = get_metrics();
    let total_processing_time = metrics
        .get_or_create_gauge("total_processing_time", "Total time taken to process a frame. From the moment we started to create this frame, until we started to send it.")
        .unwrap();

    let emission_time = metrics
        .get_or_create_gauge("emission_time", "Total time taken to emit a frame. From the moment we started to send this frame, until we finished sending it.")
        .unwrap();

    let frame_drops_before_emission = metrics
        .get_or_create_gauge("frame_drops_before_emission", "Number of dropped frames.")
        .unwrap();

    let frames_to_emit = metrics
        .get_or_create_gauge(
            "frames_to_emit",
            "Number of frames that we selected for emission.",
        )
        .unwrap();

    let mut max_send_time: u64 = 0;
    let mut _max_presentation_time: u64 = 0;

    loop {
        // Get the current time
        let since_the_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        let current_time = since_the_epoch.as_micros() as u64;

        let frame_opt = {
            let mut buffer_lock = frame_buffer.lock().unwrap();

            if !disable_frame_drops {
                // Loop to drop frames older than the max send time
                // This makes sure that only newer frames are emitted.
                while let Some(frame) = buffer_lock.front() {
                    let send_time = frame.send_time;

                    // Check if the frame is too old, meaning it's older than the current max presentation time.
                    if send_time <= max_send_time && !buffer_lock.is_empty() {
                        debug!("Dropped a frame that was older than a previously emitted frame");
                        frame_drops_before_emission.inc();
                        // This is non-ideal, but we assume that our clients their buffers are
                        // not large enough and thus could have already rendered the previously emitted frame.
                        // As such, this frame has become redundant and we can safely drop it to prevent unnecessary bandwidth usage.
                        buffer_lock.pop_front(); // Remove the outdated frame
                    } else {
                        break; // Exit the loop if no more old frames are found
                    }
                }
            }

            // Check if there's a frame ready to be emitted
            if let Some(frame) = buffer_lock.front() {
                // TODO2: maybe add ability to overwrite the presentation time of the frame
                // At this point, we already know that the frame is not older than any previously emitted frame, so we can safely overwrite the presentation time, as long as we make sure that the new presentation time is not smaller than the max_presentation_time.
                // We could dynamically adjust the presentation time based on the actual
                // time that it takes on avg to emit a frame + the avg encoding time
                // + the some other artificial offset
                // The goal of course is to minimize the delay between the send time and the presentation time
                // e.g. We could detect if the emission time keeps increasing, then we could increase that artificial offset. If it is decreasing, we could decrease it.
                // There must be a smart algorithm or mathematic formula to detect the optimal offset.
                // Also we should use the average encoding and emit times instead of the encoding and emit times of that specific frame, otherwise if the encoding time is lower then a previous frame, our presentation time could be lower than the previous frame, while the initial send time was higher.
                // CAUTION: This is a complex problem and should be handled with care, as it could lead to issues such as jitter or frame drops.

                // Get the presentation time of the frame
                let presentation_time = frame.presentation_time;
                // If the frame is too old and there are more than 1 frames in the buffer, drop it
                // We hope that the next frame is newer
                // If there is only 1 frame in the buffer, we'll emit it anyway
                // TODO: we should continue dropping such that we can catch up with the latest frame
                // TODO: we should keep track of the presentation time of the latest emitted frame and drop frames that are older than that
                if !disable_frame_drops && presentation_time < current_time && buffer_lock.len() > 1
                {
                    buffer_lock.pop_front();
                    debug!(
                        "Dropped frame with presentation time: {}",
                        presentation_time
                    );
                    frame_drops_before_emission.inc();
                    None
                } else {
                    Some(buffer_lock.pop_front().unwrap())
                }
            } else {
                None
            }
        };

        // Emit the frame if available
        if let Some(mut frame) = frame_opt {
            frames_to_emit.inc();

            // Update the max send time and presentation time
            max_send_time = frame.send_time;
            _max_presentation_time = frame.presentation_time;
            // Get the current time
            let since_the_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards");
            let current_time = since_the_epoch.as_micros() as u64;
            total_processing_time.set((current_time - frame.send_time) as i64);

            let emit_start = Instant::now();

            // Update the send time, to be used by the client metrics
            frame.send_time = current_time;

            // Emit the frame
            emit_frame_data(frame);

            let time_to_emit_frame = emit_start.elapsed().as_micros() as i64;
            emission_time.set(time_to_emit_frame);

            //debug!("Emitted frame");

            // TODO: If the time to emit took longer than the frame duration, we could adjust the FPS or reduce the max number of points.
            // Most likely we should reduce the max number of points.
            // Or, we could drop the next frame. This is a trade-off between latency and quality.
            // If we drop the next frame, we can keep the quality high, but the latency will increase.
            // If we reduce the max number of points, the quality will decrease, but the latency will be lower.
            // Do note that this logic should be done per egress, as they all function differently and each have their own prefered solution to the problem above.

            thread::sleep(Duration::from_millis(1));
        } else {
            // debug!("No frames available to emit");
            // Sleep to prevent busy-waiting
            thread::sleep(Duration::from_millis(5));
        }
    }
}

pub trait EgressProtocol: Send + Sync {
    fn encoding_format(&self) -> EncodingFormat;

    fn max_number_of_primitives(&self) -> u64;

    fn ensure_threads_started(&self);

    // Enqueue a decoded spatial frame for processing
    // It will be aggregated and encoded
    //#[instrument(skip_all)]
    #[allow(unused_variables)]
    fn push_spatial_frame(&self, spatial_frame: SpatialFrameData, stream_id: String);

    // Fast path to push a pre-encoded frame
    // This is used when we want to bypass the ring buffer
    // Or when we want to bypass the aggregation.
    //#[instrument(skip_all)]
    #[allow(unused_variables, clippy::too_many_arguments)]
    fn push_encoded_frame(
        &self,
        raw_data: Vec<u8>,
        stream_id: String,
        creation_time: u64,
        presentation_time: u64,
        ring_buffer_bypass: bool,
        payload_metadata: Option<FramePayloadMetadata>,
        client_id: Option<u64>,
        quality_index: Option<u32>,
    );

    /// Emits frame data
    //#[instrument(skip_all)]
    #[allow(unused_variables)]
    fn emit_frame_data(&self, frame: FrameTaskData);

    //#[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_fps(&self, fps: u32);

    //#[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_encoding_format(&self, encoding_format: EncodingFormat);

    //#[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_max_number_of_primitives(&self, max_number_of_primitives: u64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_gsplat_payload_is_tagged_as_gaussian_splats() {
        let payload = b"GSP\x01\x01payload";
        let metadata = infer_payload_metadata(payload);

        assert_eq!(metadata.container, FramePayloadContainer::Raw);
        assert_eq!(metadata.primitive, FrameRenderPrimitive::GaussianSplats);

        let frame = FrameTaskData {
            send_time: 1,
            presentation_time: 2,
            payload_metadata: metadata,
            data: payload.to_vec(),
            client_id: None,
            quality_index: None,
        };
        let wire = frame_task_to_pcf_wire(&frame).unwrap();
        let header = PcfHeader::parse(&wire).unwrap();

        assert_eq!(
            header.render_primitive,
            Some(PcfRenderPrimitive::GaussianSplats)
        );
        assert_eq!(header.payload, payload);
    }

    #[test]
    fn pcf_wrapped_gsplat_payload_without_primitive_is_inferred_from_payload() {
        let payload = b"GSP\x01\x01payload";
        let mut pcf_frame = Vec::new();
        PcfHeader::write_frame_to(
            &mut pcf_frame,
            &PcfFrameMeta {
                codec_magic: Some(*b"GSP"),
                ..Default::default()
            },
            payload,
        )
        .unwrap();

        let metadata = infer_payload_metadata(&pcf_frame);
        assert_eq!(metadata.container, FramePayloadContainer::Pcf);
        assert_eq!(metadata.primitive, FrameRenderPrimitive::GaussianSplats);

        let frame = FrameTaskData {
            send_time: 1,
            presentation_time: 2,
            payload_metadata: metadata,
            data: pcf_frame,
            client_id: None,
            quality_index: None,
        };
        let wire = frame_task_to_pcf_wire(&frame).unwrap();
        let header = PcfHeader::parse(&wire).unwrap();

        assert_eq!(
            header.render_primitive,
            Some(PcfRenderPrimitive::GaussianSplats)
        );
        assert_eq!(header.payload, payload);
    }

    #[test]
    fn explicit_metadata_tags_zstd_payload_as_gaussian_splats() {
        let payload = b"ZSTcompressed-ply-payload";
        let frame = FrameTaskData {
            send_time: 1,
            presentation_time: 2,
            payload_metadata: FramePayloadMetadata {
                container: FramePayloadContainer::Raw,
                primitive: FrameRenderPrimitive::GaussianSplats,
            },
            data: payload.to_vec(),
            client_id: None,
            quality_index: None,
        };

        let wire = frame_task_to_pcf_wire(&frame).unwrap();
        let header = PcfHeader::parse(&wire).unwrap();

        assert_eq!(
            header.render_primitive,
            Some(PcfRenderPrimitive::GaussianSplats)
        );
        assert_eq!(header.payload, payload);
    }
}

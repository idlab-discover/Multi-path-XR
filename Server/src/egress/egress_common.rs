use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::encoders::EncodingFormat;
use crate::processing::aggregator::PointCloudAggregator;
use crate::processing::ProcessingPipeline;
use shared_utils::types::{FrameTaskData, PointCloudData};
use circular_buffer::CircularBuffer;
use metrics::get_metrics;
use prometheus::IntGauge;
//use rayon::ThreadPoolBuilder;
use tracing::{debug, error, warn, instrument};

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
            .get_or_create_gauge("pc_combination_time", "Time taken to generate a combined point_cloud")
            .unwrap();

        let pc_encoding_time = metrics
            .get_or_create_gauge("pc_encoding_time", "Time taken to encode a combined point_cloud")
            .unwrap();

        let bytes_to_send = metrics
            .get_or_create_gauge("bytes_to_send", "Number of bytes to send")
            .unwrap();

        let number_of_combined_frames = metrics
            .get_or_create_gauge("number_of_combined_frames", "Number of combined frames generated and pushed to the egress buffer based on the frames in the aggregator")
            .unwrap();

        let frame_drops_full_egress_buffer = metrics
            .get_or_create_gauge("frame_drops_full_egress_buffer", "Number of dropped frames due to a full egress buffer.")
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

/// Starts the generator thread that periodically generates combined point clouds
/// and encodes them into frames.
#[instrument(skip_all, fields(egress_name = %egress_name))]
pub fn start_generator_thread(
    egress_name: String,
    processing_pipeline: Arc<ProcessingPipeline>,
    aggregator: Arc<PointCloudAggregator>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
) {
    let processing_pipeline_clone = processing_pipeline.clone();
    let aggregator_clone = aggregator.clone();
    let frame_buffer_clone = frame_buffer.clone();
    let fps_clone = fps.clone();
    let encoding_format_clone = encoding_format.clone();
    let max_number_of_points_clone = max_number_of_points.clone();
    let egress_name_clone = egress_name.clone();
    let thread_name = format!("{} Generator Thread", egress_name);
    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        generate_and_send_combined_point_clouds(
            egress_name_clone,
            processing_pipeline_clone,
            aggregator_clone,
            frame_buffer_clone,
            fps_clone,
            encoding_format_clone,
            max_number_of_points_clone,
        );
    });
}

/// Periodically generates combined point clouds and encodes them into frames.
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn generate_and_send_combined_point_clouds(
    egress_name: String,
    processing_pipeline: Arc<ProcessingPipeline>,
    aggregator: Arc<PointCloudAggregator>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
) {
    let current_in_queue = Arc::new(Mutex::new(0));
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
            .num_threads(thread_count)
            .build()
            .expect("Failed to build thread pool"),
    );
    */

    loop {
        let fps_value = *fps.lock().unwrap();
        let frame_duration = Duration::from_micros(1_000_000 / fps_value as u64);
        let start_time = Instant::now();

        // There may not be more then 500 ms of frames in the queue
        // First, calculate the max number of frames that can be in the queue
        let max_frame_delay = 500; // ms
        let max_frame_count_in_queue = ((max_frame_delay / (1000 / fps_value)) as i32).min(thread_count.try_into().unwrap());
        // Then, check if the current in queue count is greater than the max frame count in queue
        let current_in_queue_clone = current_in_queue.clone();
        let current_in_queue_clone = *current_in_queue_clone.lock().unwrap();
        if current_in_queue_clone > max_frame_count_in_queue {
            warn!("Frame generation is too slow, skipping frame generation. There are {} frames in the queue.", current_in_queue_clone);
            thread::sleep(frame_duration);
            continue;
        }

        // Generate the point cloud for the egress
        let generate_start_time = start_time;
        handle_point_cloud_generation(
            &egress_name,
            &processing_pipeline,
            &aggregator,
            &frame_buffer,
            &encoding_format,
            &max_number_of_points,
            &current_in_queue,
            &pc_combination_time,
            &pc_encoding_time,
            &bytes_to_send,
            &number_of_combined_frames,
            &frame_drops_full_egress_buffer,
            generate_start_time,
            false // to do; add ring buffer bypass
        );

        let processing_time = start_time.elapsed();

        // Calculate the time to sleep to maintain consistent FPS
        let sleep_duration = if frame_duration > processing_time {
            frame_duration - processing_time
        } else {
            Duration::from_millis(0)
        };

        // Sleep for the remaining time
        if !sleep_duration.is_zero() {
            //debug!("We have to wait for {:?}", sleep_duration);
            thread::sleep(sleep_duration);
        } else {
            // If processing took longer than the frame duration, log a warning
            warn!(
                "Processing time exceeded frame duration by {:?}",
                processing_time - frame_duration
            );
            // Optionally, we might want to adjust FPS or take other actions here, such as lowering the max number of points
        }
    }
}

/// Handles frame generation and encoding.
#[allow(clippy::too_many_arguments)]
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn handle_point_cloud_generation(
    egress_name: &str,
    processing_pipeline: &Arc<ProcessingPipeline>,
    aggregator: &Arc<PointCloudAggregator>,
    frame_buffer: &Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    encoding_format: &Arc<Mutex<EncodingFormat>>,
    max_number_of_points: &Arc<Mutex<u64>>,
    current_in_queue: &Arc<Mutex<i32>>,
    pc_combination_time: &IntGauge,
    pc_encoding_time: &IntGauge,
    bytes_to_send: &IntGauge,
    number_of_combined_frames: &IntGauge,
    frame_drops_full_egress_buffer: &IntGauge,
    generate_start_time: Instant,
    ring_buffer_bypass: bool,
) {
    // debug!("Handling combined point cloud generation");

    // Increment the in-queue count with a short-lived lock
    {
        // We work inside a tiny scope block to release the lock as soon as possible
        let mut current_in_queue_lock = current_in_queue.lock().unwrap();
        *current_in_queue_lock += 1;
    }

    // Generate the combined point cloud
    let max_points = *max_number_of_points.lock().unwrap();
    let combined_point_cloud = aggregator.generate_combined_point_cloud(max_points);

    pc_combination_time.set(generate_start_time.elapsed().as_micros() as i64);                

    // If the combined point cloud is empty, then skip
    if combined_point_cloud.points.is_empty() {
        // debug!("Combined point cloud is empty, skipping frame encoding");
        // Decrease the current in queue count
        let mut current_in_queue = current_in_queue.lock().unwrap();
        *current_in_queue -= 1;
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
    let ring_buffer_bypass = ring_buffer_bypass.clone();
    thread_pool.spawn(move || {
        encode_point_cloud(
            egress_name,
            combined_point_cloud,
            processing_pipeline,
            frame_buffer,
            encoding_format,
            current_in_queue,
            pc_encoding_time,
            bytes_to_send,
            number_of_combined_frames,
            frame_drops_full_egress_buffer,
            ring_buffer_bypass
        );
    });


}

// Encode the combined point cloud
#[allow(clippy::too_many_arguments)]
#[instrument(skip_all, fields(egress_name = %egress_name))]
fn encode_point_cloud(
    egress_name: String,
    combined_point_cloud: PointCloudData,
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    current_in_queue: Arc<Mutex<i32>>,
    pc_encoding_time: IntGauge,
    bytes_to_send: IntGauge,
    number_of_combined_frames: IntGauge,
    frame_drops_full_egress_buffer: IntGauge,
    _ring_buffer_bypass: bool,
) {

    debug!("Encoding combined point cloud");
    let encoding_start_time = Instant::now();

    let encoding_format = *encoding_format.lock().unwrap();
    let encoded_point_cloud = processing_pipeline.encode(combined_point_cloud, encoding_format);
    match encoded_point_cloud {
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
    let mut current_in_queue = current_in_queue.lock().unwrap();
    *current_in_queue -= 1;
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
        debug!("({}) Frame buffer is full, dropping oldest frame", egress_name);
        frame_drops_full_egress_buffer.inc();
    }
    buffer.push_back(frame);
    number_of_combined_frames.inc();
    debug!("({}) Pushed encoded frame to buffer", egress_name);
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
    ring_buffer_bypass: Option<Box<dyn Fn(FrameTaskData) + Send + 'static>>,
    bytes_to_send: IntGauge,
    number_of_combined_frames: IntGauge,
    frame_drops_full_egress_buffer: IntGauge,
    sfu_client_id: Option<u64>,
    sfu_tile_index: Option<u32>,
) {
    let data_length = data.len();

    // Build a new FrameTaskData using the provided times and data
    let frame = FrameTaskData {
        send_time: creation_time,
        presentation_time,
        data: data.into(), // Move the data into the struct
        sfu_client_id,
        sfu_frame_len: Some(data_length.try_into().unwrap_or(0)),
        sfu_tile_index
    };

    // Reuse the same ring-buffer push function
    push_encoded_frame_data(
        egress_name,
        frame_buffer,
        frame,
        ring_buffer_bypass,
        &bytes_to_send,
        &frame_drops_full_egress_buffer,
        &number_of_combined_frames,
    );
}

/// Starts the transmission thread that sends frames to clients.
#[instrument(skip_all, fields(egress_name = %egress_name))]
pub fn start_transmission_thread<F>(
    egress_name: String,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    emit_frame_data: F,
    disable_frame_drops: bool
) where
    F: Fn(FrameTaskData) + Send + 'static + Clone,
{
    let frame_buffer_clone = frame_buffer.clone();
    let emit_frame_data_clone = emit_frame_data.clone();
    let egress_name_clone = egress_name.clone();
    let thread_name = format!("{} Transmission Thread", egress_name);


    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        send_frames_to_clients(egress_name_clone, frame_buffer_clone, emit_frame_data_clone, disable_frame_drops);
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
        .get_or_create_gauge("frames_to_emit", "Number of frames that we selected for emission.")
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
                    if send_time <= max_send_time && buffer_lock.len() >= 1 {
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
                if !disable_frame_drops && presentation_time < current_time && buffer_lock.len() > 1 {
                    buffer_lock.pop_front();
                    debug!("Dropped frame with presentation time: {}", presentation_time);
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

            debug!("Emitted frame");


            // TODO: If the time to emit took longer than the frame duration, we should adjust the FPS or reduce the max number of points.
            // Most likely we should reduce the max number of points.
            // Or, we could drop the next frame. This is a trade-off between latency and quality.
            // If we drop the next frame, we can keep the quality high, but the latency will increase.
            // If we reduce the max number of points, the quality will decrease, but the latency will be lower.

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

    fn max_number_of_points(&self) -> u64;

    fn ensure_threads_started(&self);

    // Enqueue a decoded point cloud for processing
    // It will be aggregated and encoded
    #[instrument(skip_all)]
    #[allow(unused_variables)]
    fn push_point_cloud(&self, point_cloud: PointCloudData, stream_id: String);

    // Fast path to push a pre-encoded frame
    // This is used when we want to bypass the ring buffer
    // Or when we want to bypass the aggregation.
    #[instrument(skip_all)]
    #[allow(unused_variables, clippy::too_many_arguments)]
    fn push_encoded_frame(&self, raw_data: Vec<u8>, stream_id: String, creation_time: u64, presentation_time: u64, ring_buffer_bypass: bool, client_id: Option<u64>, tile_index: Option<u32>);

    /// Emits frame data
    #[instrument(skip_all)]
    #[allow(unused_variables)]
    fn emit_frame_data(&self, frame: FrameTaskData);

    #[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_fps(&self, fps: u32);

    #[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_encoding_format(&self, encoding_format: EncodingFormat);

    #[instrument(skip_all)]
    #[allow(unused_variables)]
    fn set_max_number_of_points(&self, max_number_of_points: u64);
}
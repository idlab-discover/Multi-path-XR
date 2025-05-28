// egress/websocket.rs

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::encoders::EncodingFormat;
use crate::processing::{aggregator::PointCloudAggregator, ProcessingPipeline};
use crate::services::stream_manager::StreamManager;
use shared_utils::types::{FrameTaskData, PointCloudData};

use circular_buffer::CircularBuffer;
use serde_json::Value;
use tokio::runtime::{self, Runtime};
use tracing::{debug, error, instrument};
use bytes::Bytes;
use rbase64;

use super::egress_common::{push_preencoded_frame_data, EgressCommonMetrics, EgressProtocol};

/// WebSocket Egress module responsible for sending frames over WebSocket connections.
#[derive(Clone, Debug)]
pub struct WebSocketEgress {
    stream_manager: Arc<StreamManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<PointCloudAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
    emit_with_ack: Arc<Mutex<bool>>,
    runtime: Arc<Mutex<Option<Runtime>>>,
    egress_metrics: Arc<EgressCommonMetrics>,
}

impl WebSocketEgress {
    /// Initializes the WebSocket Egress module.
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let aggregator = Arc::new(PointCloudAggregator::new(stream_manager.clone()));

        let runtime = None;

        let instance = Arc::new(Self {
            stream_manager: stream_manager.clone(),
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(Mutex::new(30)),
            encoding_format: Arc::new(Mutex::new(EncodingFormat::Draco)),
            max_number_of_points: Arc::new(Mutex::new(100000)),
            emit_with_ack: Arc::new(Mutex::new(true)),
            runtime: Arc::new(Mutex::new(runtime)),
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
        });

        // Store the instance in the StreamManager
        stream_manager.set_websocket_egress(instance.clone());
    }

    /// Sets whether to emit frames with acknowledgment.
    #[instrument(skip_all)]
    pub fn set_emit_with_ack(&self, emit_with_ack: bool) {
        *self.emit_with_ack.lock().unwrap() = emit_with_ack;
    }
}


impl EgressProtocol for WebSocketEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        *self.encoding_format.lock().unwrap()
    }

    #[inline]
    fn max_number_of_points(&self) -> u64 {
        *self.max_number_of_points.lock().unwrap()
    }

    fn ensure_threads_started(&self) {
        let already_started = self.threads_started.load(Ordering::Relaxed);
        if already_started {
            return;
        }

        // Set the threads as started
        self.threads_started.store(true, Ordering::Relaxed);

        // Start background threads using the common module
        crate::egress::egress_common::start_generator_thread(
            "WS_E".to_string(),
            self.processing_pipeline.clone(),
            self.aggregator.clone(),
            self.frame_buffer.clone(),
            self.fps.clone(),
            self.encoding_format.clone(),
            self.max_number_of_points.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "WS_E".to_string(),
            self.frame_buffer.clone(),
            move |frame| {
                self_clone.emit_frame_data(frame);
            },
            false,
        );
    }

    fn push_point_cloud(&self, point_cloud: PointCloudData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator.update_point_cloud(stream_id, point_cloud);
    }

    // Process and sends a frame, this raw version bypasses the aggregation
    fn push_encoded_frame(&self, raw_data: Vec<u8>, _stream_id: String, mut creation_time: u64, presentation_time: u64, ring_buffer_bypass: bool, client_id: Option<u64>, tile_index: Option<u32>) {
        // Ensure the threads are started
        self.ensure_threads_started();

        let self_clone = self.clone();
        let bypass = if ring_buffer_bypass {

            let since_the_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards");
            creation_time = since_the_epoch.as_micros() as u64;

            Some(Box::new(move |frame| {
                self_clone.emit_frame_data(frame);
            }) as Box<dyn Fn(FrameTaskData) + Send + 'static>)
        } else {
            None
        };
        
        // Then call the “push_preencoded_frame_data”:
        push_preencoded_frame_data(
            "WS_E",
            &self.frame_buffer,
            creation_time,
            presentation_time,
            raw_data, // data is moved
            bypass,
            self.egress_metrics.bytes_to_send.clone(),
            self.egress_metrics.frame_drops_full_egress_buffer.clone(),
            self.egress_metrics.number_of_combined_frames.clone(),
            client_id,
            tile_index,
        );
    }

    /// Emits frame data to all connected WebSocket clients.
    fn emit_frame_data(&self, frame: FrameTaskData) {
        debug!(
            "Emitting frame with presentation time: {}",
            frame.presentation_time
        );

        let emit_with_ack = *self.emit_with_ack.lock().unwrap();

        let io_option = self.stream_manager.get_socket_io();
        let io = match io_option {
            Some(io) => io,
            None => {
                error!("Socket IO is not initialized");
                return;
            }
        };

        // Convert to base64 bytes using the bitcode and rbase64 crates
        let bytes: Bytes = {
            let bytes_vec: Vec<u8> = bitcode::encode(&frame);
            let base64_encoded: String = rbase64::encode(&bytes_vec);
            Bytes::from(base64_encoded)
        };
        debug!("Bytes created");
        debug!("Encoded frame to {} bytes", bytes.len());

        if emit_with_ack {
            // Calculate the difference between the send time and the presentation time
            let presentation_offset = if frame.send_time <= frame.presentation_time {
                frame.presentation_time.saturating_sub(frame.send_time)
            } else {
                u64::MAX - 500
            };
            // The timeout should be the min of 800ms and the presentation offset + 500
            let timeout = Duration::from_millis(std::cmp::min(800, presentation_offset + 500));
            // Emit the frame with acknowledgment
            debug!(
                "Emitting frame with acknowledgment and timeout: {:?}",
                timeout
            );

            // Check that at least one client is connected
            if io.sockets().unwrap().is_empty() {
                debug!("No clients connected to emit frame");
                return;
            }

            // Check if the runtime already exists
            let mut runtime_guard = self.runtime.lock().unwrap();
            if runtime_guard.is_none() {
                *runtime_guard = Some(runtime::Builder::new_multi_thread().worker_threads(2).thread_name_fn(|| {
                    static ATOMIC_WS_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let id = ATOMIC_WS_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    format!("WS_R w-{}", id)
                }).enable_all().build().unwrap());
            }
            runtime_guard.as_ref().unwrap().block_on(async {

                match io
                    .to("broadcast")
                    .timeout(timeout)
                    .emit_with_ack::<Bytes, Value>(
                        "frame:broadcast:ack",
                        &bytes,
                    ) {
                    Ok(ack_stream) => match ack_stream.await {
                        Ok(_) => debug!(
                            "Ack received for frame with presentation time: {}",
                            frame.presentation_time
                        ),
                        Err(err) => error!("Ack error: {:?}", err),
                    },
                    Err(err) => {
                        error!("Socket error during emit with ack: {:?}", err);
                    }
                }
            });
        } else {
            debug!("Emitting frame without acknowledgment");

            // Emit the frame without acknowledgment
            match io.to("broadcast").emit::<Bytes>(
                "frame:broadcast",
                &bytes,
            ) {
                Ok(_) => debug!(
                    "Frame emitted without acknowledgment with presentation time: {}",
                    frame.presentation_time
                ),
                Err(err) => error!("Socket error during emit without ack: {:?}", err),
            }
        }
    }

    fn set_fps(&self, fps: u32) {
        *self.fps.lock().unwrap() = fps;
    }

    fn set_encoding_format(&self, encoding_format: EncodingFormat) {
        *self.encoding_format.lock().unwrap() = encoding_format;
    }

    fn set_max_number_of_points(&self, max_number_of_points: u64) {
        *self.max_number_of_points.lock().unwrap() = max_number_of_points;
    }
}
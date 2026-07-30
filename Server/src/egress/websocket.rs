// egress/websocket.rs

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::processing::{aggregator::SpatialFrameAggregator, ProcessingPipeline};
use crate::services::stream_manager::StreamManager;
use shared_utils::types::{FramePayloadMetadata, FrameTaskData, SpatialFrameData};
use spatial_codecs::encoder::EncodingFormat;

use bytes::Bytes;
use circular_buffer::CircularBuffer;
use rbase64;
use serde_json::Value;
use tokio::runtime::{self, Runtime};
use tracing::{debug, error, instrument};

use super::egress_common::{
    frame_task_to_pcf_wire, push_preencoded_frame_data, AtomicEncodingFormat, EgressCommonMetrics,
    EgressProtocol,
};

/// WebSocket Egress module responsible for sending frames over WebSocket connections.
#[derive(Clone, Debug)]
pub struct WebSocketEgress {
    stream_manager: Arc<StreamManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<SpatialFrameAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
    encoding_format: Arc<AtomicEncodingFormat>,
    max_number_of_primitives: Arc<AtomicU64>,
    emit_with_ack: Arc<AtomicBool>,
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
        let aggregator = Arc::new(SpatialFrameAggregator::new(stream_manager.clone()));

        let runtime = None;

        let instance = Arc::new(Self {
            stream_manager: stream_manager.clone(),
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(AtomicU32::new(30)),
            encoding_format: Arc::new(AtomicEncodingFormat::new(EncodingFormat::Draco)),
            max_number_of_primitives: Arc::new(AtomicU64::new(100000)),
            emit_with_ack: Arc::new(AtomicBool::new(true)),
            runtime: Arc::new(Mutex::new(runtime)),
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
        });

        // Store the instance in the StreamManager
        stream_manager.set_websocket_egress(instance.clone());
    }

    /// Sets whether to emit frames with acknowledgment.
    #[instrument(skip_all)]
    pub fn set_emit_with_ack(&self, emit_with_ack: bool) {
        self.emit_with_ack.store(emit_with_ack, Ordering::Relaxed);
    }
}

impl EgressProtocol for WebSocketEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        self.encoding_format.load()
    }

    #[inline]
    fn max_number_of_primitives(&self) -> u64 {
        self.max_number_of_primitives.load(Ordering::Relaxed)
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
            self.max_number_of_primitives.clone(),
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

    fn push_spatial_frame(&self, spatial_frame: SpatialFrameData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator
            .update_spatial_frame(stream_id, spatial_frame);
    }

    // Process and sends a frame, this raw version bypasses the aggregation
    fn push_encoded_frame(
        &self,
        raw_data: Vec<u8>,
        _stream_id: String,
        mut creation_time: u64,
        presentation_time: u64,
        ring_buffer_bypass: bool,
        payload_metadata: Option<FramePayloadMetadata>,
        client_id: Option<u64>,
        quality_index: Option<u32>,
    ) {
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
            })
                as Box<dyn Fn(FrameTaskData) + Send + 'static>)
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
            payload_metadata,
            bypass,
            self.egress_metrics.as_ref(),
            client_id,
            quality_index,
        );
    }

    /// Emits frame data to all connected WebSocket clients.
    fn emit_frame_data(&self, frame: FrameTaskData) {
        debug!(
            "Emitting frame with presentation time: {}",
            frame.presentation_time
        );

        let emit_with_ack = self.emit_with_ack.load(Ordering::Relaxed);

        let io_option = self.stream_manager.get_socket_io();
        let io = match io_option {
            Some(io) => io,
            None => {
                error!("Socket IO is not initialized");
                return;
            }
        };

        // Socket.IO can split binary-looking payloads on control bytes in this path, so keep PCF base64-wrapped here.
        let bytes: Bytes = {
            let bytes_vec = match frame_task_to_pcf_wire(&frame) {
                Ok(bytes_vec) => bytes_vec,
                Err(err) => {
                    error!("Failed to encode frame as PCF: {}", err);
                    return;
                }
            };
            let base64_encoded: String = rbase64::encode(&bytes_vec);
            Bytes::from(base64_encoded)
        };
        debug!("Bytes created");
        debug!("Encoded frame to {} bytes", bytes.len());

        // Calculate the difference between the send time and the presentation time
        let presentation_offset = if frame.send_time <= frame.presentation_time {
            frame.presentation_time.saturating_sub(frame.send_time)
        } else {
            u64::MAX - 500
        };
        // The timeout should be the min of 800ms and the presentation offset + 500
        let timeout = Duration::from_millis(std::cmp::min(800, presentation_offset + 500));

        // Check that at least one client is connected
        if io.sockets().is_empty() {
            debug!("No clients connected to emit frame");
            return;
        }

        // Check if the runtime already exists
        let mut runtime_guard = self.runtime.lock().unwrap();
        if runtime_guard.is_none() {
            *runtime_guard = Some(
                runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name_fn(|| {
                        static ATOMIC_WS_ID: std::sync::atomic::AtomicUsize =
                            std::sync::atomic::AtomicUsize::new(0);
                        let id = ATOMIC_WS_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        format!("WS_R w-{id}")
                    })
                    .enable_all()
                    .build()
                    .unwrap(),
            );
        }
        runtime_guard.as_ref().unwrap().block_on(async {
            if emit_with_ack {
                // Emit the frame with acknowledgment
                debug!(
                    "Emitting frame with acknowledgment and timeout: {:?}",
                    timeout
                );
                match io
                    .to("broadcast")
                    .timeout(timeout)
                    .emit_with_ack::<Bytes, Value>("frame:broadcast:ack", &bytes)
                    .await
                {
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
            } else {
                debug!("Emitting frame without acknowledgment");

                // Emit the frame without acknowledgment
                match io
                    .to("broadcast")
                    .emit::<Bytes>("frame:broadcast", &bytes)
                    .await
                {
                    Ok(_) => debug!(
                        "Frame emitted without acknowledgment with presentation time: {}",
                        frame.presentation_time
                    ),
                    Err(err) => error!("Socket error during emit without ack: {:?}", err),
                }
            }
        });
    }

    fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::Relaxed);
    }

    fn set_encoding_format(&self, encoding_format: EncodingFormat) {
        self.encoding_format.store(encoding_format);
    }

    fn set_max_number_of_primitives(&self, max_number_of_primitives: u64) {
        self.max_number_of_primitives
            .store(max_number_of_primitives, Ordering::Relaxed);
    }
}

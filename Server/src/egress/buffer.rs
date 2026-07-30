// Server/src/egress/buffer.rs

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::timing::micros_to_timescale_units;
use crate::{
    processing::{aggregator::SpatialFrameAggregator, ProcessingPipeline},
    services::{
        mpd_manager::{MpdManager, PayloadLengths},
        stream_manager::StreamManager,
    },
};
use circular_buffer::CircularBuffer;
use mp4_box::writer::{create_media_segment, Mp4StreamConfig};
use shared_utils::types::{FramePayloadMetadata, FrameTaskData, SpatialFrameData};
use spatial_codecs::encoder::EncodingFormat;
use tokio::{sync::Notify, time::sleep};
use tracing::{debug, error, instrument};

use super::egress_common::{
    frame_task_to_pcf_wire, push_preencoded_frame_data, AtomicEncodingFormat, EgressCommonMetrics,
    EgressProtocol,
};

#[derive(Clone, Debug)]
pub struct BufferFrame {
    pub index: u64,
    pub data: Vec<u8>,
}

type BufferStorage = (CircularBuffer<60, BufferFrame>, u64, Mp4StreamConfig);

#[derive(Clone, Debug)]
pub struct BufferEgress {
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<SpatialFrameAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
    encoding_format: Arc<AtomicEncodingFormat>,
    max_number_of_primitives: Arc<AtomicU64>,
    egress_metrics: Arc<EgressCommonMetrics>,
    circular_storages: Arc<Mutex<HashMap<String, BufferStorage>>>, // TODO: should be dashmap or a 'rwlock with an inner mutex'
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    mpd_manager: Arc<MpdManager>,
}

impl BufferEgress {
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
        mpd_manager: Arc<MpdManager>,
    ) {
        let aggregator = Arc::new(SpatialFrameAggregator::new(stream_manager.clone()));

        let instance = Arc::new(Self {
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(AtomicU32::new(30)),
            encoding_format: Arc::new(AtomicEncodingFormat::new(EncodingFormat::Draco)),
            max_number_of_primitives: Arc::new(AtomicU64::new(100000)),
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
            circular_storages: Arc::new(Mutex::new(HashMap::new())),
            notifiers: Arc::new(Mutex::new(HashMap::new())),
            mpd_manager,
        });

        stream_manager.set_buffer_egress(instance.clone());
    }

    pub fn get_stream_config(&self, stream_id: &str) -> Option<Mp4StreamConfig> {
        let storages = self.circular_storages.lock().unwrap();
        storages.get(stream_id).map(|(_, _, config)| config.clone())
    }

    pub async fn get_frame(
        &self,
        stream_id: &str,
        index: u64,
        timeout: Duration,
    ) -> Result<BufferFrame, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        // per-stream notifier (create if missing)
        let notify = {
            let mut ns = self.notifiers.lock().unwrap();
            ns.entry(stream_id.to_string())
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone()
        };
        loop {
            {
                let storages = self.circular_storages.lock().unwrap();
                if let Some((storage, _, _)) = storages.get(stream_id) {
                    if let Some(frame) = storage.iter().find(|f| f.index == index).cloned() {
                        return Ok(frame);
                    }

                    // We haven't found the frame yet, let's wait a little
                    // Maybe it will be added later

                    // For speed, we will assume that the first frame is the oldest
                    // And thus has the lowest index
                    let min_index = storage.front().map(|f| f.index);

                    if let Some(min) = min_index {
                        // The requested index is lower than the minimum index in the buffer
                        // This means that the frame will never be added
                        if index < min {
                            return Err("Frame index is out of bounds".into());
                        }
                    }

                    // TODO: we could also predict if the frame will ever be added within the given timeout period.
                } else {
                    return Err("Stream not found".into());
                }
            };

            // Check if we have reached the timeout
            // If we have, return None
            if tokio::time::Instant::now() >= deadline {
                return Err("Timeout reached".into());
            }

            // Wait until a new frame is pushed or until near the deadline
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err("Timeout reached".into());
            }
            // Small safety cap: wake periodically at <=5 ms to re-check deadline
            let cap = Duration::from_millis(5);
            tokio::select! {
                _ = notify.notified() => {},
                _ = sleep(remaining.min(cap)) => {},
            }
        }
    }

    #[allow(dead_code)]
    pub async fn get_latest_frame_index(&self, stream_id: &str) -> Option<u64> {
        let storages = self.circular_storages.lock().unwrap();
        if let Some((storage, _, _)) = storages.get(stream_id) {
            if let Some(frame) = storage.back() {
                return Some(frame.index);
            }
        }
        None
    }

    pub async fn get_first_and_last_frame_indices(&self, stream_id: &str) -> Option<(u64, u64)> {
        let storages = self.circular_storages.lock().unwrap();
        if let Some((storage, _, _)) = storages.get(stream_id) {
            if let (Some(first), Some(last)) = (storage.front(), storage.back()) {
                return Some((first.index, last.index));
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn clear_stream(&self, stream_id: &str) {
        let mut storages = self.circular_storages.lock().unwrap();
        storages.remove_entry(stream_id);
    }

    pub fn get_mpd(&self, group_id: &str) -> Option<String> {
        self.mpd_manager.get_mpd(group_id)
    }

    pub fn get_groups(&self) -> Vec<String> {
        self.mpd_manager.get_groups()
    }
}

impl EgressProtocol for BufferEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        self.encoding_format.load()
    }

    #[inline]
    fn max_number_of_primitives(&self) -> u64 {
        self.max_number_of_primitives.load(Ordering::Relaxed)
    }

    fn ensure_threads_started(&self) {
        if self.threads_started.load(Ordering::Relaxed) {
            return;
        }

        self.threads_started.store(true, Ordering::Relaxed);

        crate::egress::egress_common::start_generator_thread(
            "BUF_E".to_string(),
            self.processing_pipeline.clone(),
            self.aggregator.clone(),
            self.frame_buffer.clone(),
            self.fps.clone(),
            self.encoding_format.clone(),
            self.max_number_of_primitives.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "BUF_E".to_string(),
            self.frame_buffer.clone(),
            move |frame| {
                self_clone.emit_frame_data(frame);
            },
            true,
        );
    }

    fn push_spatial_frame(&self, spatial_frame: SpatialFrameData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator
            .update_spatial_frame(stream_id, spatial_frame);
    }

    fn push_encoded_frame(
        &self,
        raw_data: Vec<u8>,
        _stream_id: String,
        mut creation_time: u64,
        presentation_time: u64,
        _ring_buffer_bypass: bool,
        payload_metadata: Option<FramePayloadMetadata>,
        client_id: Option<u64>,
        quality_index: Option<u32>,
    ) {
        self.ensure_threads_started();

        // The buffer egress will always bypass the ring buffer
        // This is because the emission will just result in a push to a different buffer.
        let ring_buffer_bypass = true;
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

        push_preencoded_frame_data(
            "BUF_E",
            &self.frame_buffer,
            creation_time,
            presentation_time,
            raw_data,
            payload_metadata,
            bypass,
            self.egress_metrics.as_ref(),
            client_id,
            quality_index,
        );
    }

    fn emit_frame_data(&self, frame: FrameTaskData) {
        let stream_id = format!(
            "client_{}_{}",
            frame.client_id.unwrap_or(0),
            frame.quality_index.unwrap_or(0)
        );
        let group_id = format!("client_{}_", frame.client_id.unwrap_or(0));
        let codec = frame.data[0..3].to_ascii_lowercase();
        let codec_string = String::from_utf8_lossy(&codec).into_owned();
        let encoded = match frame_task_to_pcf_wire(&frame) {
            Ok(encoded) => encoded,
            Err(err) => {
                error!("Failed to encode DASH frame as PCF: {}", err);
                return;
            }
        };
        let encoded_len = encoded.len();
        let fps = self.fps.load(Ordering::Relaxed).max(1);
        let mut created_stream = false;
        let segment_len;

        {
            let mut storages = self.circular_storages.lock().unwrap();

            // Check if the stream already exists
            if !storages.contains_key(&stream_id) {
                // Create the Mp4StreamConfig
                let config = Mp4StreamConfig {
                    timescale: fps * 1000,
                    width: 1920, // Example defaults
                    height: 1080,
                    codec_fourcc: [codec[0], codec[1], codec[2], b' '],
                    track_id: frame.quality_index.unwrap_or(0) + 1, // The track ID starts at 1, so we add 1
                    default_sample_duration: 1000, // This will be divided by the timescale
                    codec_name: format!("SpatialCodec_{codec_string}"),
                };

                // Find the next available index within the group
                let next_index = storages
                    .iter()
                    .filter(|(key, _)| key.starts_with(&group_id))
                    .map(|(_, (_, index, _))| *index)
                    .max()
                    .unwrap_or(0);

                // Insert a new circular buffer and index
                storages.insert(
                    stream_id.clone(),
                    (CircularBuffer::new(), next_index, config),
                );
                created_stream = true;
            }

            // Get a mutable reference to the stream
            let (buffer, index, config) = storages.get_mut(&stream_id).unwrap();

            // Decode time is the timeline position in timescale units.
            let decode_time = micros_to_timescale_units(frame.presentation_time, config.timescale);
            let segment_bytes = create_media_segment(
                config,
                encoded, // Use the encoded Bytes directly
                *index as u32,
                decode_time,
            );
            segment_len = segment_bytes.len();

            if created_stream {
                self.mpd_manager.add_stream_to_mpd(
                    &group_id,
                    &stream_id,
                    "video/pc",
                    &codec_string,
                    PayloadLengths {
                        content_len_bytes: encoded_len,
                        network_payload_len_bytes: segment_len,
                    },
                    fps as u64,
                );
            }

            // Construct the buffer frame
            let buffer_frame = BufferFrame {
                index: *index,
                data: segment_bytes,
            };

            // Increment the index and store the frame
            *index += 1;
            buffer.push_back(buffer_frame);

            debug!(
                "Stored frame in buffer of stream {} at index {}",
                stream_id,
                *index - 1
            );

            // Wake any waiters for this stream
            if let Some(n) = self.notifiers.lock().unwrap().get(&stream_id).cloned() {
                n.notify_waiters();
            }
        }

        if !created_stream {
            self.mpd_manager.update_stream_bandwidth(
                &group_id,
                &stream_id,
                PayloadLengths {
                    content_len_bytes: encoded_len,
                    network_payload_len_bytes: segment_len,
                },
            );
        }
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

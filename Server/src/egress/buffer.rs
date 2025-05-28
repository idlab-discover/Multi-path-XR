// Server/src/egress/buffer.rs

use std::{collections::HashMap, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, time::{Duration, SystemTime, UNIX_EPOCH}};

use crate::{encoders::EncodingFormat, processing::{aggregator::PointCloudAggregator, ProcessingPipeline}, services::{mpd_manager::MpdManager, stream_manager::StreamManager}};
use mp4_box::writer::{create_media_segment, Mp4StreamConfig};
use shared_utils::types::{FrameTaskData, PointCloudData};
use circular_buffer::CircularBuffer;
use bytes::Bytes;
use tokio::time::sleep;
use tracing::{debug, instrument};

use super::egress_common::{push_preencoded_frame_data, EgressCommonMetrics, EgressProtocol};

#[derive(Clone, Debug)]
pub struct BufferFrame {
    pub index: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct BufferEgress {
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<PointCloudAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
    egress_metrics: Arc<EgressCommonMetrics>,
    circular_storages: Arc<Mutex<HashMap<String, (CircularBuffer<60, BufferFrame>, u64, Mp4StreamConfig)>>>,
    mpd_manager: Arc<MpdManager>,
}

impl BufferEgress {
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
        mpd_manager: Arc<MpdManager>,
    ) {
        let aggregator = Arc::new(PointCloudAggregator::new(stream_manager.clone()));

        let instance = Arc::new(Self {
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(Mutex::new(30)),
            encoding_format: Arc::new(Mutex::new(EncodingFormat::Draco)),
            max_number_of_points: Arc::new(Mutex::new(100000)),
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
            circular_storages: Arc::new(Mutex::new(HashMap::new())),
            mpd_manager
        });

        stream_manager.set_buffer_egress(instance.clone());
    }

    pub fn get_stream_config(&self, stream_id: &str) -> Option<Mp4StreamConfig> {
        let storages = self.circular_storages.lock().unwrap();
        storages.get(stream_id).map(|(_, _, config)| config.clone())
    }

    pub async fn get_frame(&self, stream_id: &str, index: u64, timeout: Duration) -> Option<BufferFrame> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let storages = self.circular_storages.lock().unwrap();
                if let Some((storage, _, _)) = storages.get(stream_id) {

                    if let Some(frame) = storage.iter().find(|f| f.index == index).cloned() {
                        return Some(frame);
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
                            return None;
                        }
                    }
                } else {
                    return None;
                }
            };

            // Check if we have reached the timeout
            // If we have, return None
            if tokio::time::Instant::now() >= deadline {
                return None;
            }

            // Sleep for a short duration before checking again
            sleep(Duration::from_millis(1)).await;

        }
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
        *self.encoding_format.lock().unwrap()
    }

    #[inline]
    fn max_number_of_points(&self) -> u64 {
        *self.max_number_of_points.lock().unwrap()
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
            self.max_number_of_points.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "BUF_E".to_string(),
            self.frame_buffer.clone(),
            move |frame| {
                self_clone.emit_frame_data(frame);
            },
            true
        );
    }

    fn push_point_cloud(&self, point_cloud: PointCloudData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator.update_point_cloud(stream_id, point_cloud);
    }

    fn push_encoded_frame(&self, raw_data: Vec<u8>, _stream_id: String, mut creation_time: u64, presentation_time: u64, _ring_buffer_bypass: bool, client_id: Option<u64>, tile_index: Option<u32>) {
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
            }) as Box<dyn Fn(FrameTaskData) + Send + 'static>)
        } else {
            None
        };

        push_preencoded_frame_data(
            "BUF_E",
            &self.frame_buffer,
            creation_time,
            presentation_time,
            raw_data,
            bypass,
            self.egress_metrics.bytes_to_send.clone(),
            self.egress_metrics.frame_drops_full_egress_buffer.clone(),
            self.egress_metrics.number_of_combined_frames.clone(),
            client_id,
            tile_index,
        );
    }
    
    fn emit_frame_data(&self, frame: FrameTaskData) {
        let stream_id = format!("client_{}_{}", frame.sfu_client_id.unwrap_or(0), frame.sfu_tile_index.unwrap_or(0));
        // Copy the first three bytes from the frame data
        let codec = frame.data.clone()[0..3].to_ascii_lowercase().to_vec();
        let encoded = {
            let bytes_vec: Vec<u8> = bitcode::encode(&frame);
            let base64_encoded: String = rbase64::encode(&bytes_vec);
            let bytes = Bytes::from(base64_encoded);
            bytes.to_vec()

        };

        {
            let mut storages = self.circular_storages.lock().unwrap();
        
            // Check if the stream already exists
            if !storages.contains_key(&stream_id) {
                let group_id = format!("client_{}_", frame.sfu_client_id.unwrap_or(0));
                let fps = *self.fps.lock().unwrap();
        
                // Add stream to MPD
                self.mpd_manager.add_stream_to_mpd(
                    &group_id,
                    &stream_id,
                    "video/pc",
                    &String::from_utf8_lossy(&codec),
                    encoded.len().saturating_mul(fps.try_into().unwrap()).saturating_mul(8) as u64, // Bandwidth in bits
                    fps as u64,
                );

                // Create the Mp4StreamConfig
                let config = Mp4StreamConfig {
                    timescale: fps * 1000,
                    width: 1920,   // Example defaults
                    height: 1080,
                    codec_fourcc: [codec[0], codec[1], codec[2], b' '],
                    track_id: frame.sfu_tile_index.unwrap_or(0) + 1, // The track ID starts at 1, so we add 1
                    default_sample_duration: 1000, // This will be divided by the timescale
                    codec_name: format!("PointCloudCodec_{}", String::from_utf8_lossy(&codec)),
                };
        
                // Find the next available index within the group
                let next_index = storages
                    .iter()
                    .filter(|(key, _)| key.starts_with(&group_id))
                    .map(|(_, (_, index, _))| *index)
                    .max()
                    .unwrap_or(0);
        
                // Insert a new circular buffer and index
                storages.insert(stream_id.clone(), (CircularBuffer::new(), next_index, config));
            }
        
            // Get a mutable reference to the stream
            let (buffer, index, config) = storages.get_mut(&stream_id).unwrap();

            // Decode time is the // Timeline position in timescale units
            let decode_time = frame.presentation_time * config.timescale as u64 / 1000;
            let segment_bytes = create_media_segment(
                config,
                &encoded, // Use the encoded Bytes directly
                *index as u32,
                decode_time,
            );
        
            // Construct the buffer frame
            let buffer_frame = BufferFrame {
                index: *index,
                data: segment_bytes, // TODO: instead of encoded, we should use the m4s file
            };
        
            // Increment the index and store the frame
            *index += 1;
            buffer.push_back(buffer_frame);
        
            debug!("Stored frame in buffer of stream {} at index {}", stream_id, *index - 1);
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
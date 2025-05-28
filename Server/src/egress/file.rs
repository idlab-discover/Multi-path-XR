// egress/file.rs

use std::{
    fs::{self, File}, io::Write, path::PathBuf, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}
};

use crate::{
    encoders::EncodingFormat,
    processing::{aggregator::PointCloudAggregator, ProcessingPipeline},
    services::stream_manager::StreamManager
};
use shared_utils::types::{FrameTaskData, PointCloudData};
use circular_buffer::CircularBuffer;
use tracing::{debug, error, info, instrument};

use super::egress_common::{push_preencoded_frame_data, EgressCommonMetrics, EgressProtocol};

#[derive(Clone, Debug)]
pub struct FileEgress {
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<PointCloudAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
    egress_metrics: Arc<EgressCommonMetrics>,
}

impl FileEgress {
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
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
        });

        stream_manager.set_file_egress(instance.clone());
    }
} 

impl EgressProtocol for FileEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        *self.encoding_format.lock().unwrap()
    }
    
    fn max_number_of_points(&self) -> u64 {
        *self.max_number_of_points.lock().unwrap()
    }

    fn ensure_threads_started(&self) {
        let already_started = self.threads_started.load(Ordering::Relaxed);
        if already_started {
            return;
        }

        self.threads_started.store(true, Ordering::Relaxed);

        crate::egress::egress_common::start_generator_thread(
            "FILE_E".to_string(),
            self.processing_pipeline.clone(),
            self.aggregator.clone(),
            self.frame_buffer.clone(),
            self.fps.clone(),
            self.encoding_format.clone(),
            self.max_number_of_points.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "FILE_E".to_string(),
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

    fn push_encoded_frame(&self, raw_data: Vec<u8>, _stream_id: String, mut creation_time: u64, presentation_time: u64, ring_buffer_bypass: bool, client_id: Option<u64>, tile_index: Option<u32>) {
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

        push_preencoded_frame_data(
            "FILE_E",
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
        if frame.data.len() < 3 {
            error!("Frame data is too short to contain an extension");
            return;
        }

        let ext_bytes = &frame.data[0..3];
        let extension = String::from_utf8_lossy(ext_bytes).to_lowercase();

        let mut path = PathBuf::from("dist/exports");
        let client_id = frame.sfu_client_id.map_or("unknown".to_string(), |c| c.to_string());
        let tile = frame.sfu_tile_index.map_or("unknown".to_string(), |t| t.to_string());
        let send_time = frame.send_time;
        let stream_id = format!("client_{}_{}", client_id, tile);
        info!("FileEgress: stream_id: {}", stream_id);
        path.push(stream_id);

        if let Err(e) = fs::create_dir_all(&path) {
            error!("Failed to create directory {:?}: {}", path, e);
            return;
        }

        path.push(format!("{}.{}", send_time, extension));

        match File::create(&path) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(&frame.data) {
                    error!("Failed to write frame to file {:?}: {}", path, e);
                } else {
                    debug!("Wrote frame to {:?}", path);
                }
            }
            Err(e) => {
                error!("Failed to create file {:?}: {}", path, e);
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

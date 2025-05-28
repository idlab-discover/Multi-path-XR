use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use metrics::get_metrics;
use pre_encode::prep_for_encoding;
use prometheus::IntGauge;
use rayon::ThreadPool;
use sampling::partition_by_percentages;
use crate::decoders;
use crate::encoders::{self, EncodingFormat};
use crate::services::stream_manager::StreamManager;
use tracing::{error, instrument};
use shared_utils::types::{FrameTaskData, PointCloudData};

pub mod aggregator;
pub mod filtering;
pub mod pre_encode;
pub mod sampling;

#[derive(Clone, Debug)]
pub struct ProcessingPipeline {
    pub thread_pool: Arc<ThreadPool>,
    pub decoding_time: IntGauge,
    pub process_to_buffer_time: IntGauge,
    pub frames_to_decode: IntGauge,
    
}

impl ProcessingPipeline {
    #[instrument(skip_all)]
    pub fn new(thread_pool: Arc<ThreadPool>) -> Self {
        let metrics = get_metrics();
        Self { 
            thread_pool,
            decoding_time: metrics.get_or_create_gauge(
                "decoding_time", 
                "Time taken to decode a frame").unwrap(),
            process_to_buffer_time: metrics.get_or_create_gauge(
                "process_to_buffer_time", 
                "Time taken to process a frame and push it to the egress buffer where it will be combined with the other streams.").unwrap(),
            frames_to_decode: metrics.get_or_create_gauge(
                "frames_to_decode", 
                "Number of frames to be decoded").unwrap(),
         }
    }

    #[instrument(skip_all)]
    pub fn decode(&self, raw_data: Vec<u8>) -> Result<PointCloudData, Box<dyn std::error::Error>> {
        decoders::decode_data(raw_data)
    }

    #[instrument(skip_all)]
    pub fn encode(
        &self,
        point_cloud: PointCloudData,
        encoding: EncodingFormat,
    ) -> Result<FrameTaskData, Box<dyn std::error::Error>> {
        let creation_time = point_cloud.creation_time;
        let presentation_time = point_cloud.presentation_time;
        let data = encoders::encode_data(point_cloud, encoding);

        match data {
            Ok(data) => Ok(FrameTaskData {
                send_time: creation_time,
                presentation_time,
                data,
                sfu_client_id: None,
                sfu_frame_len: None,
                sfu_tile_index: None,
            }),
            Err(e) => {
                Err(e)
            }
            
        }
        
    }

    #[instrument(skip_all)]
    pub fn push_to_decoder(
        &self,
        raw_data: Vec<u8>,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        let processing_pipeline = Arc::new(self.clone());
        let stream_manager_clone = stream_manager.clone();
        let stream_id_clone = stream_id.clone();

        let settings = stream_manager_clone.get_stream_settings(&stream_id);
        // Check if we should process this frame
        if !settings.process_incoming_frames {
            // Drop the frame
            return;
        }



        let thread_pool = Arc::clone(&self.thread_pool);
        let presentation_time_offset = settings.presentation_time_offset;

        if settings.decode_bypass {
            thread_pool.spawn(move || {
                // Instead of decoding, treat `raw_data` as “already decoded” or “raw frame”.
                // We can call a new function that directly handles raw frames:
                processing_pipeline.process_frame_raw(
                    raw_data,
                    stream_manager_clone,
                    stream_id_clone,
                );
            });
        } else {
            let decoding_time = self.decoding_time.clone();
            let process_to_buffer_time = self.process_to_buffer_time.clone();
            let frames_to_decode = self.frames_to_decode.clone();
            thread_pool.spawn(move || {
                ProcessingPipeline::handle_decoding_and_processing(
                    processing_pipeline,
                    raw_data,
                    stream_manager_clone,
                    stream_id_clone,
                    presentation_time_offset,
                    decoding_time,
                    process_to_buffer_time,
                    frames_to_decode,
                );
            });
        }
    }

    /// Handles decoding and processing of a frame in a separate thread.
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all)]
    fn handle_decoding_and_processing(
        processing_pipeline: Arc<ProcessingPipeline>,
        raw_data: Vec<u8>,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
        presentation_time_offset: Option<u64>,
        decoding_time: IntGauge,
        process_to_buffer_time: IntGauge,
        frames_to_decode: IntGauge,
    ) {

        let start_time = Instant::now();
        // Decode the raw data
        let mut point_cloud = match processing_pipeline.decode(raw_data) {
            Ok(pc) => pc,
            Err(e) => {
                error!("Decoding failed: {:?}", e);
                return;
            }
        };

        if presentation_time_offset.is_some() {
            let offset = presentation_time_offset.unwrap();
            point_cloud.presentation_time = point_cloud.creation_time.saturating_add(offset);
        }

        // Capture how long it took to decode the frame
        decoding_time.set(start_time.elapsed().as_micros() as i64);

        let start_time = Instant::now();

        // Increment the number of frames to process
        frames_to_decode.inc();
        // Process the frame
        processing_pipeline.process_frame(point_cloud, stream_manager, stream_id);

        // Capture how long it took to process the frame
        process_to_buffer_time.set(start_time.elapsed().as_micros() as i64);
    }

    #[instrument(skip_all, fields(stream_id = %stream_id))]
    pub fn process_frame(
        &self,
        point_cloud: PointCloudData,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        // Get stream settings
        let settings = stream_manager.get_stream_settings(&stream_id);
        let thread_pool = Arc::clone(&self.thread_pool);

        // Dispatch the point cloud to the egress protocols specified in the settings
        for egress in stream_manager.get_egresses(&settings.egress_protocols) {
            if settings.aggregator_bypass {
                let point_cloud_prepped = prep_for_encoding(point_cloud.clone(), &settings, Some(egress.max_number_of_points()));
                if let Some(ref percentages) = settings.max_point_percentages {
                    // Split the point cloud into disjoint sub-clouds
                    let sub_clouds = partition_by_percentages(&point_cloud_prepped.points, percentages).unwrap();
                    for (index, sub_cloud) in sub_clouds.into_iter().enumerate() {
                        let pc = PointCloudData {
                            points: sub_cloud,
                            creation_time: point_cloud.creation_time,
                            presentation_time: point_cloud.presentation_time,
                            error_count: point_cloud.error_count,
                        };
                        let tile_index = settings.sfu_tile_index.map(|index_value| index_value + index as u32);
                        let ring_buffer_bypass = settings.ring_buffer_bypass;
                        let client_id = settings.sfu_client_id;
                        // If both tile index and client id are None, we clone the stream id, otherwise we create a new stream id
                        let stream_id = if client_id.is_some() && tile_index.is_some() {
                            format!("client_{}_{}", client_id.unwrap(), tile_index.unwrap())
                        } else {
                            stream_id.clone()
                        };
                        

                        let egress_clone = egress.clone();
                        let thread_pool = thread_pool.clone();
                        let processing_pipeline_clone = self.clone();


                        thread_pool.spawn(move || {
                            let bytes = processing_pipeline_clone.encode(pc.clone(), egress_clone.encoding_format()).unwrap().data;
                            egress_clone.push_encoded_frame(
                                bytes,
                                stream_id,
                                pc.creation_time,
                                pc.presentation_time,
                                ring_buffer_bypass,
                                client_id,
                                tile_index,
                            );
                        });
                    }
                } else {
                    let bytes = self.encode(point_cloud_prepped.clone(), egress.encoding_format()).unwrap().data;
                    egress.push_encoded_frame(
                        bytes,
                        stream_id.clone(),
                        point_cloud.creation_time,
                        point_cloud.presentation_time,
                        settings.ring_buffer_bypass,
                        settings.sfu_client_id,
                        settings.sfu_tile_index,
                    );
                }
            } else {
                egress.push_point_cloud(point_cloud.clone(), stream_id.clone());
            }
        }
    }

    /// Called when `decode_bypass = true`.
    /// We treat `raw_data` as though it’s “the final data” to pass on.
    #[instrument(skip_all)]
    pub fn process_frame_raw(
        &self,
        raw_data: Vec<u8>,
        stream_manager: Arc<StreamManager>,
        stream_id: String,
    ) {
        // We skip decoding entirely.
        // Now proceed as if we had a "point cloud" or "raw chunk" to egress.

        let settings = stream_manager.get_stream_settings(&stream_id);


        // Get the current time
        let since_the_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
        let creation_time = since_the_epoch.as_micros() as u64;
        let presentation_time_offset = settings.presentation_time_offset;
        let presentation_time = if presentation_time_offset.is_some() {
            creation_time.saturating_add(presentation_time_offset.unwrap())
        } else {
            100
        };

        let ring_buffer_bypass = settings.ring_buffer_bypass;
        let client_id = settings.sfu_client_id;
        let tile_index = settings.sfu_tile_index;

        // Push the encoded frame to all the requested egress protocols
        for egress in stream_manager.get_egresses(&settings.egress_protocols) {
            egress.push_encoded_frame(raw_data.clone(), stream_id.clone(), creation_time, presentation_time, ring_buffer_bypass, client_id, tile_index);
        }
    }
}

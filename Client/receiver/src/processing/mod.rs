use std::{sync::{Arc, Mutex}, time::{SystemTime, UNIX_EPOCH}};
use crate::{storage::Storage, types::FrameData};
use crate::processing::decoders::decode_data;
use rayon::{ThreadPoolBuilder, ThreadPool};
use tokio::runtime::{Builder, Runtime};
use tracing::{debug, error};

pub mod decoders;

pub struct ProcessingPipeline {
    storage: Arc<Storage>,
    thread_pool: Arc<ThreadPool>,
    pub runtime: Arc<Mutex<Runtime>>,
    disable_parser: bool,
}

impl ProcessingPipeline {
    pub fn new(storage: Arc<Storage>, thread_count: usize, disable_parser: bool) -> Self {// Initialize thread pool
        let thread_pool = Arc::new(
            ThreadPoolBuilder::new()
                .num_threads(thread_count)
                .build()
                .expect("Failed to build thread pool"),
        );
        let runtime = Arc::new(Mutex::new(
            Builder::new_multi_thread()
                .thread_name_fn(|| {
                    static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    format!("PP_R w-{}", id)
                })
                .enable_all()
                .build()
                .expect("Failed to build runtime"),
        ));

        Self {
            storage,
            thread_pool,
            runtime,
            disable_parser
        }
    }


    pub fn ingest_data(&self, stream_id: String, quality: u64, send_time: u64, presentation_time: u64, data: Vec<u8>) {
        let storage = self.storage.clone();
        let thread_pool = self.thread_pool.clone();
        let disable_parser = self.disable_parser;

        storage.quality_metric.set(quality as i64);

        thread_pool.spawn(move || {
            // info!("Processing frame data for stream_id: {} and send_time {}, length: {}", stream_id, send_time, presentation_time);
            let start_time = SystemTime::now();
            let frame_data = if disable_parser {
                Ok(FrameData {
                    send_time,
                    presentation_time,
                    receive_time: 0,
                    error_count: 0,
                    point_count: 1,
                    coordinates: vec![0.0, 0.0, 0.0],
                    colors: vec![255, 255, 255],
                })
            } else {
                decode_data(send_time, presentation_time, data.to_owned())
            };
            match frame_data {
                Ok(mut frame_data) => {
                    if frame_data.error_count > 0 {
                        error!("Frame data has errors (stream_id: {}, error_count: {})", stream_id, frame_data.error_count);
                    }
                    // Check that the frame data has at least one point
                    if frame_data.point_count == 0 {
                        debug!("Frame data has no points (stream_id: {})", stream_id);
                        return;
                    }
                    let end_time = SystemTime::now();
                    let decode_duration = match end_time.duration_since(start_time) {
                        Ok(duration) => duration.as_micros() as u64,
                        Err(e) => {
                            error!("Failed to calculate decode duration: {:?}", e);
                            return;
                        }
                    };
                    storage.clone().decode_time.set(decode_duration as i64);


                    frame_data.receive_time = start_time.duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
                    let send_to_receive = frame_data.receive_time.saturating_sub(frame_data.send_time);
                    storage.clone().send_to_receive_time_diff.set(send_to_receive as i64);

                    storage.insert_frame(stream_id, frame_data);
                }
                Err(e) => {
                    error!("Failed to decode frame data: {:?}", e);
                }
                
            };
        });
    }
}

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use dash_player::{DashPlayer, DashEvent};
use mp4_box::reader::extract_mdat_boxes;
use shared_utils::types::FrameTaskData;
use tokio::{runtime::Runtime, task::JoinHandle};
use tracing::{debug, error, warn};
use crate::{
    processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
};

pub struct DashIngress {
    url: String,
    pub group_map: Arc<RwLock<HashMap<String, JoinHandle<()>>>>,
    pub stream_manager: Arc<StreamManager>,
    pub processing_pipeline: Arc<ProcessingPipeline>,
    pub runtime: Arc<Mutex<Runtime>>,
}

impl DashIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let url = stream_manager.websocket_url.read().unwrap().clone();
        if url.is_none() {
            error!("URL is empty");
            return;
        }


        let runtime = Arc::clone(&processing_pipeline.runtime);
        let ingress = Arc::new(Self {
            url: url.unwrap(),
            group_map: Arc::new(RwLock::new(HashMap::new())),
            stream_manager: stream_manager.clone(),
            processing_pipeline,
            runtime
        });


        // Keep a reference to ourselves in the StreamManager
        stream_manager.set_dash_ingress(ingress);
    }

    pub fn spawn_group(
        &self,
        group_id: String,
    ) {
        let runtime = Arc::clone(&self.runtime);

        runtime.lock().unwrap().block_on(async {
            // Wait 1 second, then spawn. This makes sure that all the representations are available in the backend.
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            self.spawn_group_tokio(group_id);
        });
    }

    fn spawn_group_tokio(&self, group_id: String) {
        if self.group_map.read().unwrap().contains_key(&group_id) {
            debug!("DASH player for group_id '{}' already exists", group_id);
            return;
        }

        debug!("Spawning DASH player for group_id '{}'", group_id);

        let stream_id = format!("dash_{}", group_id);
        let mpd_url = format!("{}/dash/{}.mpd", &self.url, group_id);
        let pipeline = Arc::clone(&self.processing_pipeline);
        let group_id_clone = group_id.clone(); // clone for move into task

        let handle = tokio::spawn(async move {
            let cb_pipeline = Arc::clone(&pipeline);
            let cb_stream_id = stream_id.clone();
            let cb_group_id = group_id_clone.clone();

            let callback = move |event: DashEvent| {
                let cb_pipeline = Arc::clone(&cb_pipeline);
                let cb_stream_id = cb_stream_id.clone();
                let cb_group_id = cb_group_id.clone();

                tokio::spawn(async move {
                    match event {
                        DashEvent::Segment {
                            data,
                            content_type,
                            representation_id,
                            segment_number,
                            url,
                            playback_rate,
                            ..
                        } => {
                            debug!(
                                "DASH [{} - {}] - segment {} (type: {}, rate: {}) size: {} bytes",
                                cb_group_id,
                                representation_id,
                                segment_number,
                                content_type,
                                playback_rate,
                                data.len()
                            );

                            if url.ends_with("init.mp4") {
                                return;
                            }

                            //info!(url);
                            //info!("First 16 bytes: {:?}", &data[..16.min(data.len())]);

                            // Use fast mdat extractor
                            let mdat_boxes = match extract_mdat_boxes(&data) {
                                Ok(boxes) => boxes,
                                Err(err) => {
                                    warn!("Failed to parse mdat boxes: {}", err);
                                    return;
                                }
                            };

                            if mdat_boxes.is_empty() {
                                warn!("No mdat boxes found in segment {}", segment_number);
                                return;
                            }



                            let quality = {
                                // Split the representation id on '_' and take the last part
                                let parts: Vec<&str> = representation_id.split('_').collect();
                                // Get the last part and parse it as u64
                                match parts.last() {
                                    Some(last_part) => match last_part.parse::<u64>() {
                                        Ok(quality) => quality,
                                        Err(_) => 0
                                    },
                                    None => 0
                                }
                            };


                            for mdat in mdat_boxes {
                                let mdat_data = mdat.data;
                                if mdat_data.is_empty() {
                                    warn!("Empty mdat box found");
                                    continue;
                                }

                                // Decode the payload
                                let bytes_str = match std::str::from_utf8(&mdat_data) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        warn!("Invalid UTF-8 sequence: {}", e);
                                        continue;
                                    }
                                };
                                let bytes_decoded = match rbase64::decode(bytes_str) {
                                    Ok(decoded) => decoded,
                                    Err(err) => {
                                        warn!("Failed to decode payload: {}", err);
                                        continue;
                                    }
                                };
                                let frame_task_data = match bitcode::decode::<FrameTaskData>(&bytes_decoded) {
                                    Ok(decoded) => decoded,
                                    Err(err) => {
                                        warn!("Failed to decode payload: {}", err);
                                        continue;
                                    }
                                };

                                cb_pipeline.ingest_data(
                                    cb_stream_id.clone(),
                                    quality,
                                    frame_task_data.send_time,
                                    frame_task_data.presentation_time,
                                    frame_task_data.data,
                                );
                            }
                        }
                        DashEvent::Info(msg) => debug!("DASH [{}] Info: {}", cb_group_id, msg),
                        DashEvent::Warning(msg) => error!("DASH [{}] Warning: {}", cb_group_id, msg),
                        DashEvent::DownloadError { url, reason } => {
                            error!("DASH [{}] DownloadError: {} - {}", cb_group_id, url, reason)
                        }
                    }
                });
            };

            let callback_arc = Arc::new(callback);
            match DashPlayer::new(&mpd_url, callback_arc).await {
                Ok(player) => {
                    player.set_target_latency(0.001).await;
                    if let Err(e) = player.start().await {
                        error!("DASH [{}] Failed to start player: {}", group_id_clone, e);
                    }
                }
                Err(e) => {
                    error!("DASH [{}] Failed to create player: {}", group_id_clone, e);
                }
            }
        });

        self.group_map.write().unwrap().insert(group_id, handle);
    }
}

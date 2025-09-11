use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use dash_player::{DashPlayer, DashEvent};
use mp4_box::reader::extract_mdat_boxes;
use shared_utils::types::FrameTaskData;
use tokio::{runtime::Runtime, task::JoinHandle};
use tracing::{debug, error, info, warn};
use crate::{
    processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
};

pub struct DashIngress {
    url: String,
    pub group_map: Arc<RwLock<HashMap<String, (JoinHandle<()>, Arc<DashPlayer>)>>>,
    // pub stream_manager: Weak<StreamManager>,
    pub processing_pipeline: Arc<ProcessingPipeline>,
    pub runtime: Arc<Mutex<Option<Runtime>>>,
}
crate::log_drop!(DashIngress);

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
            // stream_manager: Arc::downgrade(&stream_manager),
            processing_pipeline,
            runtime
        });


        // Keep a reference to ourselves in the StreamManager
        stream_manager.set_dash_ingress(ingress);
    }

    pub fn stop(&self) {
        info!("Stopping DASH ingress");

        // Stop all active players
        let mut group_map = self.group_map.write().unwrap();
        for (group_id, (handle, player)) in group_map.drain() {
            info!("Stopping DASH player for group_id '{}'", group_id);
            player.stop();
            handle.abort();
        }

        // Clear the group map
        group_map.clear();
    }

    pub fn spawn_group(
        &self,
        group_id: String,
    ) {
        if let Some(rt) = self.runtime.lock().unwrap().as_ref() {
            rt.block_on(async {
                // Wait 1 second, then spawn. This makes sure that all the representations are available in the backend.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                self.spawn_group_tokio(group_id);
            });
        }
    }

    fn spawn_group_tokio(&self, group_id: String) {
        if self.group_map.read().unwrap().contains_key(&group_id) {
            debug!("DASH player for group_id '{}' already exists", group_id);
            return;
        }

        debug!("Spawning DASH player for group_id '{}'", group_id);

        let stream_id = group_id.to_string();
        let mpd_url = format!("{}/dash/{}.mpd", &self.url, group_id);
        let pipeline = Arc::clone(&self.processing_pipeline);
        let group_id_clone = group_id.clone(); // clone for move into task
        let group_map = Arc::clone(&self.group_map); // Clone for move into task

        let callback = Arc::new(move |event: DashEvent| {
            let cb_pipeline = Arc::clone(&pipeline);
            let cb_stream_id = stream_id.clone();
            let cb_group_id = group_id_clone.clone();

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
                            debug!("No mdat boxes found in segment {}", segment_number);
                            return;
                        }



                        let quality = {
                            // Split the representation id on '_' and take the last part
                            let parts: Vec<&str> = representation_id.split('_').collect();
                            // Get the last part and parse it as u64
                            match parts.last() {
                                Some(last_part) => last_part.parse::<u64>().unwrap_or_default(), // Default to 0 if parsing fails
                                None => 0
                            }
                        };


                        for mdat in mdat_boxes {
                            let mdat_data = mdat.data;
                            if mdat_data.is_empty() {
                                debug!("Empty mdat box found");
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
                    DashEvent::EmptySegment { segment_number } => {
                        debug!("DASH [{}] EmptySegment: {}", cb_group_id, segment_number);
                        cb_pipeline.empty_frame(cb_stream_id.clone());
                    }
                    DashEvent::Info(msg) => info!("DASH [{}] Info: {}", cb_group_id, msg),
                    DashEvent::Warning(msg) => warn!("DASH [{}] Warning: {}", cb_group_id, msg),
                    DashEvent::DownloadError { url, reason } => {
                        error!("DASH [{}] DownloadError: {} - {}", cb_group_id, url, reason)
                    }
                }
            });
        });

        // Spawn task to create player and its own task
        tokio::spawn(async move {
            match DashPlayer::new(&mpd_url, callback).await {
                Ok(player) => {
                    player.set_target_latency(0.001).await;
                    let player = Arc::new(player);
                    let group_id_clone = group_id.clone();
                    let player_clone = Arc::clone(&player);

                    let handle = tokio::spawn(async move {
                        if let Err(e) = player_clone.start().await {
                            error!("DASH [{}] Failed to start player: {}", group_id_clone, e);
                        }
                    });

                    group_map.write().unwrap().insert(group_id, (handle, player));
                }
                Err(e) => {
                    error!("DASH [{}] Failed to create player: {}", group_id, e);
                }
            }
        });
    }

    pub fn set_fetching_enabled(&self, group_id: &str, enabled: bool) {
        if let Some((_, player)) = self.group_map.read().unwrap().get(group_id) {
            player.set_fetching_enabled(enabled);
        }
    }
}

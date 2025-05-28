// egress/webrtc.rs

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::encoders::EncodingFormat;
use crate::processing::aggregator::PointCloudAggregator;
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use crate::types::{WebRtcIceCandidate, WebRtcOffer};

use shared_utils::codec::video_codec_capability;
use shared_utils::peer_connection::create_webrtc_peer_connection;
use shared_utils::types::{FrameTaskData, PointCloudData};

use circular_buffer::CircularBuffer;
use serde_json::Value;
use socketioxide::extract::SocketRef;
use tokio::runtime::{self, Runtime};
use tracing::{debug, error, info, instrument};

use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::track::track_local::TrackLocal;

use shared_utils::track_local_pointcloud_rtp::TrackLocalPointCloudRTP;

use super::egress_common::{push_preencoded_frame_data, EgressCommonMetrics, EgressProtocol};

static WEBRTC_RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

/// WebRTC Egress module responsible for sending frames over WebRTC data channels.
#[derive(Clone)]
pub struct WebRTCEgress {
    stream_manager: Arc<StreamManager>,
    tracks: Arc<RwLock<HashMap<String, Arc<TrackLocalPointCloudRTP>>>>,
    rtp_senders: Arc<RwLock<HashMap<String, HashMap<String, Arc<RTCRtpSender>>>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    aggregator: Arc<PointCloudAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
    /// The map of all connected PeerConnections: socket_id -> RTCPeerConnection
    peer_connections: Arc<RwLock<HashMap<String, Arc<RTCPeerConnection>>>>,
    /// Temporary storage of ICE candidates if the `remote_description` is not yet set
    pending_ice: Arc<RwLock<HashMap<String, Vec<RTCIceCandidateInit>>>>,
    egress_metrics: Arc<EgressCommonMetrics>,
}

impl fmt::Debug for WebRTCEgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebRTCEgress")
            .field("tracks", &"<hidden>") // Replace with something meaningful or omit
            .field("processing_pipeline", &self.processing_pipeline)
            .field("frame_buffer", &self.frame_buffer)
            .field("aggregator", &self.aggregator)
            .field("fps", &self.fps)
            .field("encoding_format", &self.encoding_format)
            .field("max_number_of_points", &self.max_number_of_points)
            .field("egress_metrics", &self.egress_metrics)
            .finish()
    }
}

impl WebRTCEgress {
    /// Initializes the WebRTC Egress module.
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let aggregator = Arc::new(PointCloudAggregator::new(stream_manager.clone()));

        let instance = Arc::new(Self {
            stream_manager: stream_manager.clone(),
            tracks: Arc::new(RwLock::new(HashMap::new())),
            rtp_senders: Arc::new(RwLock::new(HashMap::new())),
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(Mutex::new(30)),
            encoding_format: Arc::new(Mutex::new(EncodingFormat::Draco)),
            max_number_of_points: Arc::new(Mutex::new(100000)),
            peer_connections: Arc::new(RwLock::new(HashMap::new())),
            pending_ice: Arc::new(RwLock::new(HashMap::new())),
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
        });

        // Store the instance in the StreamManager
        stream_manager.set_webrtc_egress(instance.clone());
    }

    #[instrument(skip_all)]
    pub fn get_runtime(&self) -> Arc<Runtime> {
        WEBRTC_RUNTIME.get_or_init(|| {
            let rt = runtime::Builder::new_multi_thread()
                //.worker_threads(2)
                .thread_name_fn(|| {
                    static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    format!("WRTC_R w-{}", id)
                })
                .enable_all()
                .build().unwrap();
            Arc::new(rt)
        }).clone()
    }
    
    pub fn add_rtp_sender(&self, track_id: String, client_id: String, rtp_sender: Arc<RTCRtpSender>) {
        info!("New WebRTC track created for client: {} with id: {}", client_id.clone(), track_id.clone());
        // Check if the channel already exists
        let mut rtp_senders = self.rtp_senders.write().unwrap();
        let track_senders = rtp_senders.get_mut(&track_id.clone());
        if let Some(track_senders) = track_senders {
            track_senders.insert(client_id, rtp_sender.clone());

        } else {
            // Push a new hash map for the client
            // First we create the hash map and push the track to it
            let mut new_senders = HashMap::new();
            new_senders.insert(client_id, rtp_sender.clone());
            rtp_senders.insert(track_id.clone(), new_senders);
        }
    }


    #[allow(dead_code)]
    pub fn remove_rtp_sender_for_track(&self, track_id: &str, client_id: &str) {
        let mut rtp_senders = self.rtp_senders.write().unwrap();
        if let Some(track_senders) = rtp_senders.get_mut(track_id) {
            track_senders.remove(client_id);
            if track_senders.is_empty() {
                rtp_senders.remove(track_id);
            }
        }
    }

    pub fn remove_rtp_senders_for_client(&self, client_id: &str) {
        let mut rtp_senders = self.rtp_senders.write().unwrap();
        let mut tracks_ids_to_remove = Vec::new();
        let mut binding = rtp_senders.clone();
        // Remove the tracks that belong to the client
        for client_tracks in binding.iter_mut() {
            let rtp_sender = client_tracks.1.remove(client_id);
            if let Some(rtp_sender) = rtp_sender {
                // Close the sender
                // Get the peer connection
                if let Some(peer_connection) = self.peer_connections.read().unwrap().get(client_id).cloned() {
                    // Get the runtime
                    let runtime = self.get_runtime();
                    // Close the peer connection
                    let peer_connection_clone = peer_connection.clone();
                    let rtp_sender_clone = rtp_sender.clone();
                    runtime.spawn(async move {
                        peer_connection_clone.remove_track(&rtp_sender_clone).await.unwrap();
                    });
                }
            }

            if client_tracks.1.is_empty() {
                tracks_ids_to_remove.push(client_tracks.0);
            }
        }
        // Remove all hash maps that do not contain any tracks
        for track_id_to_remove in tracks_ids_to_remove {
            rtp_senders.remove(track_id_to_remove.as_str());
            // Also remove the track from the tracks
            self.remove_track(track_id_to_remove.as_str());
        }
    }

    // Remove all tracks with the given track id
    pub fn remove_track(&self, track_id: &str) {
        let mut tracks = self.tracks.write().unwrap();
        tracks.remove(track_id);
    }

    // Get all the tracks accross the different clients with the given track id
    pub fn get_or_create_track(&self, track_id: &str) -> Arc<TrackLocalPointCloudRTP> {
        {
            let tracks = self.tracks.read().unwrap();
            if let Some(track) = tracks.get(track_id) {
                return track.clone();
            }
        }

        // If not found, create and insert it
        let fps = {
            let fps = *self.fps.lock().unwrap();
            fps
        };

        let new_track = Arc::new(
            TrackLocalPointCloudRTP::new(
                video_codec_capability(),
                track_id.to_owned(),
                "0".to_owned(),
                fps,
            )
        );

        let mut tracks = self.tracks.write().unwrap();
        tracks.insert(track_id.to_string(), new_track.clone());

        new_track
    }

    /// Removes all tracks for a client and close the 
    pub fn close_peer_connection(&self, client_id: &str) {
        {
            // Get the peer connection
            if let Some(peer_connection) = self.peer_connections.read().unwrap().get(client_id).cloned() {
                // Get the runtime
                let runtime = self.get_runtime();
                // Close the peer connection
                let peer_connection_clone = peer_connection.clone();
                runtime.spawn(async move {
                    peer_connection_clone.close().await.unwrap();
                });
            }
        }

        self.remove_rtp_senders_for_client(client_id);
        self.peer_connections.write().unwrap().remove(client_id);
        self.pending_ice.write().unwrap().remove(client_id);

        // Get the ingress and remove the data channel
        if let Some(ingress) = self.stream_manager.get_webrtc_ingress() {
            ingress.remove_data_channel(client_id);
        }
    }

    #[instrument(skip_all)]
    pub async fn handle_client_offer(
        self: Arc<Self>,
        socket_id: String,
        offer: WebRtcOffer,
        socket: SocketRef,
    ) -> Result<(), Box<dyn std::error::Error>> {

        // 1) Create PeerConnection
        let pc = create_webrtc_peer_connection().await?;

        // 2) **Forward server-side ICE to client**:  
        //    Whenever the server finds a new ICE candidate,
        //    it sends it to the client as `webrtc_ice_candidate`.
        let s_clone = socket.clone();
        pc.on_ice_candidate(Box::new(move |cand: Option<RTCIceCandidate>| {
            let s_clone = s_clone.clone();
            Box::pin(async move {
                if let Some(c) = cand {
                    if let Ok(json_candidate) = c.to_json() {
                        let json_val = serde_json::json!({
                            "candidate": json_candidate.candidate,
                            "sdpMid": json_candidate.sdp_mid,
                            "sdpMLineIndex": json_candidate.sdp_mline_index,
                        });
                        // Send to the client
                        let _ = s_clone
                        .emit("webrtc_ice_candidate", &json_val);
                    }
                }
            })
        }));

        pc.on_track(Box::new(move | _track, _receiver, _transceiver| {
            Box::pin(async move {
                info!("Created new remote track");
                // TODO: we should store this track somewhere
            })
        }));

        // 3) Get or reate a broadcast track
        let track_id = "client_0_0".to_string();
        let broadcast_track = self.get_or_create_track(&track_id.clone());

        // Add the track to the PeerConnection
        let rtp_sender = pc.add_track(broadcast_track.clone() as Arc<dyn TrackLocal + Send + Sync>).await?;


        // Read incomming RTCP packets
        // Before these packets are returned, they are processed by interceptors.
        // This is required for things such as NACK and RTCP feedback.
        let rtp_sender_clone = rtp_sender.clone();
        tokio::spawn(async move {
            let mut rtcp_buffer = vec![0; 1500];
            while let Ok((_, _)) = rtp_sender_clone.read(&mut rtcp_buffer).await {}
            Result::<_, ()>::Ok(())
        });

        // Set the handler for Peer connection state
        // This will notify you when the peer has connected/disconnected
        let socket_id_clone = socket_id.clone();
        let track_id_clone = track_id.clone();
        let self_clone = self.clone();
        pc.on_peer_connection_state_change(Box::new(
            move |s: RTCPeerConnectionState| {
                info!("Peer Connection State has changed: {s}");
                if s == RTCPeerConnectionState::Connected {
                    // 9) Store the broadcast track to the tracks
                    self_clone.add_rtp_sender(track_id_clone.clone(), socket_id_clone.clone(), rtp_sender.clone());
                }
                Box::pin(async move {})
            },
        ));

        debug!("Created new PeerConnection for client: {} with sdp: {}", socket_id.clone(), offer.sdp);

        // 4) Set remote description from the client’s SDP
        let offer_sdp = serde_json::from_str::<RTCSessionDescription>(&offer.sdp);
        if let Err(e) = offer_sdp {
            error!("Failed to create offer SDP: {}", e);
            return Err(Box::new(e));
        }
        let offer_sdp = offer_sdp.unwrap();
        pc.set_remote_description(offer_sdp).await.unwrap();
        // 5) Create and send back an answer
        let answer: RTCSessionDescription = pc.create_answer(None).await.unwrap();
        {
            let payload = serde_json::to_string(&answer).unwrap();
            let answer_obj = serde_json::json!({
                "sdp": payload,
                "clientId": socket_id.to_string()
            });
            match socket.emit_with_ack::<Value, Value>("webrtc_answer", &answer_obj) {
                Ok(ack_stream) => {
                    match ack_stream.await {
                        Ok(_) => {
                            debug!("Sent WebRTC answer to client: {}", socket_id.clone());
                        },
                        Err(err) => {
                            error!("Ack error from socket {}: {:?}", socket_id, err);
                        },
                    }
                },
                Err(err) => {
                    error!("Socket errror when emitting WebRTC answer: {:?}", err);
                }
            };
        }

        // 6) Store our local description in the PeerConnection
        pc.set_local_description(answer.clone()).await.unwrap();

        // 7) Check if we have any pending ICE candidates
        {
            // Move them out of pending_ice_map into the peer connection
            let pending_list = {
                let mut map = self.pending_ice.write().unwrap();
                map.remove(&socket_id.clone())
            };
            if let Some(mut pending_list) = pending_list {
                for cand in pending_list.drain(..) {
                    if let Err(e) = pc.add_ice_candidate(cand).await {
                        error!("Failed to add previously cached ICE candidate: {}", e);
                    } else {
                        debug!("Added a previously cached ICE candidate for {}", socket_id.clone());
                    }
                }
            }
        }

        // TODO: both step 8 and 9 should be done inside a on function that triggers when the connection is triggered.

        // 8) Save PeerConnection in a HashMap
        {
            let mut map = self.peer_connections.write().unwrap();
            map.insert(socket_id.clone(), pc);
        }

        Ok(())
    }

    #[instrument(skip_all)]
    pub async fn handle_client_ice_candidate(
        &self,
        socket_id: String,
        candidate: WebRtcIceCandidate
    ) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Received ICE candidate from client (WS: {}): {:?}", socket_id.clone(), candidate);
        let c = RTCIceCandidateInit {
            candidate: candidate.candidate,
            sdp_mid: candidate.sdp_mid,
            sdp_mline_index: candidate.sdp_mline_index,
            ..Default::default()
        };

        let pc = {
            let map = self.peer_connections.read().unwrap();
            map.get(&socket_id).cloned()
        };

        if let Some(pc) = pc {
            let desc = pc.remote_description().await;
            if desc.is_none() {
                let mut map = self.pending_ice.write().unwrap();
                let pending_list = map.entry(socket_id.clone()).or_default();
                pending_list.push(c);
                debug!("No remote description set for {}, caching ICE candidate in the meantime.", socket_id);
            } else if let Err(e) = pc.add_ice_candidate(c).await {
                error!("Failed to add ICE candidate: {}", e);
                return Err(Box::new(e));
            }
        } else {
            let mut map = self.pending_ice.write().unwrap();
            let pending_list = map.entry(socket_id.clone()).or_default();
            pending_list.push(c);
            debug!("No peer connection found for {}, caching ICE candidate in the meantime.", socket_id);
        }

        Ok(())
    }
}


impl EgressProtocol for WebRTCEgress {
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
            "WRTC_E".to_string(),
            self.processing_pipeline.clone(),
            self.aggregator.clone(),
            self.frame_buffer.clone(),
            self.fps.clone(),
            self.encoding_format.clone(),
            self.max_number_of_points.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "WRTC_E".to_string(),
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
            "WRTC_E",
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

    /// Emits frame data to all connected WebRTC data channels.
    fn emit_frame_data(&self, frame: FrameTaskData) {
        debug!("Emitting frame with presentation time: {}", frame.presentation_time);

        let track_id = format!("client_{}_{}", frame.sfu_client_id.unwrap_or(0), frame.sfu_tile_index.unwrap_or(0));

        let track = {
            let tracks = self.tracks.read().unwrap();
            tracks.get(&track_id).cloned()
        };
        
        if track.is_none() {
            debug!("No track found with id: {}", track_id);
            return;
        }

        let track = track.unwrap();

        // debug!("Sending frame to {} tracks", selected_tracks.len());

        // Send the frame to all data channels
        let runtime = self.get_runtime();
        let frame_clone = frame.clone();
        let track_clone = track.clone();
        runtime.block_on(async move {
            let result = track_clone.write_frame(&frame_clone).await;
            if let Err(e) = result {
                error!("Failed to write frame to track: {}", e);
            }
        });
        //debug!("Frame sent to all tracks");
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
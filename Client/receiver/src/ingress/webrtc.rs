use std::{sync::{Arc, Mutex}, thread};
use tokio::{runtime::Runtime, sync::RwLock};
use tracing::{debug, error, info, instrument};
use webrtc::{ice_transport::ice_candidate::RTCIceCandidateInit, peer_connection::{
        peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection
    }, rtp_transceiver::rtp_codec::RTPCodecType
};

use crate::{
    processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
};
use shared_utils::{peer_connection::create_webrtc_peer_connection, track_remote_pointcloud_rtp::TrackRemotePointCloudRTP, types::FrameTaskData};

/// A client-side module for receiving frames via WebRTC data channel.
pub struct WebRTCIngress {
    /// Our single PeerConnection, or None if not created yet
    pc: RwLock<Option<Arc<RTCPeerConnection>>>,
    /// Reference to the StreamManager
    stream_manager: Arc<StreamManager>,
    /// Reference to the pipeline for decoding/storing frames
    pipeline: Arc<ProcessingPipeline>,
    /// Pending ICE candidates to be applied after the remote description is set
    pending_candidates: RwLock<Vec<RTCIceCandidateInit>>,
    pub runtime: Arc<Mutex<Runtime>>,
}

impl WebRTCIngress {
    /// Create a new, empty instance. Typically called once from `Ingress::initialize()`.
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        pipeline: Arc<ProcessingPipeline>,
    ) {
        let runtime = Arc::clone(&pipeline.runtime);

        let ingress = Arc::new(Self {
            pc: RwLock::new(None),
            stream_manager: stream_manager.clone(),
            pipeline,
            pending_candidates: RwLock::new(Vec::new()),
            runtime
        });
        // Keep a reference to ourselves in the StreamManager
        stream_manager.set_webrtc_ingress(ingress);
    }

    /// Actually create the PeerConnection on the client side, attach handlers, and produce an SDP offer.
    #[instrument(skip(self))]
    pub async fn create_offer(&self) -> Result<String, String> {
        // 1) Create PeerConnection
        let pc = create_webrtc_peer_connection().await?;

        // 2) **Forward client-side ICE to server**:  
        //    Whenever the client finds a new ICE candidate,
        //    it sends it to the server as `webrtc_ice_candidate`.
        let sm_clone = self.stream_manager.clone();
        pc.on_ice_candidate(Box::new(move |c| {
            let sm_clone2 = sm_clone.clone();
            Box::pin(async move {
                debug!("Client-side ICE candidate found");
                if let Some(candidate) = c {
                    if let Ok(json_candidate) = candidate.to_json() {
                        // We just store or forward it. Actual "send to server" is handled outside.
                        let json_val = serde_json::json!({
                            "candidate": json_candidate.candidate,
                            "sdpMid": json_candidate.sdp_mid,
                            "sdpMLineIndex": json_candidate.sdp_mline_index,
                        });

                        debug!("Client-side ICE candidate: {:?}", json_val.clone());

                        // Spawn a normal non-async thread and emit the ICE candidate
                        // Rustsocket-io doesn't support calling emit from an async context when we already block that context
                        thread::spawn(move || {
                            // Get a reference to the WebSocketIngress
                            let websocket_ingress = {
                                match sm_clone2.websocket_ingress.read().unwrap().as_ref() {
                                    Some(i) => i.clone(),
                                    None => {
                                        error!("WebSocketIngress not found, did you call WebSocketIngress::initialize()?");
                                        return;
                                    }
                                }
                            };
                            let socket_option = websocket_ingress.get_socket();
                            let socket_guard = socket_option.lock().unwrap();
                            let socket = match &*socket_guard {
                                Some(s) => s,
                                None => {
                                    error!("WebSocket not connected, did you call WebSocketIngress::connect()?");
                                    return;
                                }
                            };

                            if let Err(err) = socket.emit("webrtc_ice_candidate", json_val) {
                                error!("Failed to emit ICE candidate: {}", err);
                            }
                        });
                    }
                }
            })
        }));

        // Set the handler for Peer connection state
        // This will notify you when the peer has connected/disconnected
        pc.on_peer_connection_state_change(Box::new(
            move |s: RTCPeerConnectionState| {
                info!("Peer Connection State has changed: {s}");
                Box::pin(async {})
            },
        ));

        pc.add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .map_err(|e| format!("add_transceiver_from_kind failed: {e}"))?;

        let pipeline_clone = self.pipeline.clone();
        pc.on_track(Box::new(move | track, _receiver, _transceiver| {
            let p = pipeline_clone.clone();
            Box::pin(async move {
                info!("Created new track");
                let some_on_frame_cb = Arc::new(move |frame: FrameTaskData| {            
                    // info!("Received frame with {} bytes", frame.data.len());
                
                    p.ingest_data(
                        format!("client_{}_{}", frame.sfu_client_id.unwrap_or(0), frame.sfu_tile_index.unwrap_or(0)),
                        0,
                        frame.send_time,
                        frame.presentation_time,
                        frame.data);
                });
            
                let mut remote_pc_track = TrackRemotePointCloudRTP::new(track, some_on_frame_cb);
                remote_pc_track.start(); 
                // TODO: we should store this track somewhere so we can stop it when the connection is closed   
            })
        }));

        // Create the local SDP offer
        let offer = pc
            .create_offer(None)
            .await
            .map_err(|e| format!("create_offer failed: {e}"))?;

        // Set the local SDP offer
        // This should also start the gathering of ICE candidates
        pc.set_local_description(offer.clone())
            .await
            .map_err(|e| format!("set_local_description failed: {e}"))?;

        let mut guard = self.pc.write().await;
        *guard = Some(pc);

        let payload = serde_json::to_string(&offer).unwrap();
        Ok(payload)
    }

    /// Handle the server's answer (SDP).
    #[instrument(skip(self, answer_sdp))]
    pub async fn handle_answer(&self, answer_sdp: String) -> Result<(), String> {
        let pc_opt = self.pc.read().await;
        let pc = match &*pc_opt {
            Some(pc) => pc.clone(),
            None => return Err("No PeerConnection available".to_string()),
        };


        let desc = serde_json::from_str::<RTCSessionDescription>(&answer_sdp)
            .map_err(|e| format!("Invalid answer: {e}"))?;

        //info!("{:?}", desc);

        pc.set_remote_description(desc)
            .await
            .map_err(|e| format!("Failed to set remote desc: {e}"))?;

        // Handle any pending ICE candidates
        let mut candidates = self.pending_candidates.write().await;
        for candidate in candidates.drain(..) {
            info!("Adding pending ICE candidate to PeerConnection");
            pc.add_ice_candidate(candidate)
                .await
                .map_err(|e| format!("Failed to add pending ICE candidate: {e}"))?;
        }

        Ok(())
    }

    /// Handle ICE candidates from the server.  
    /// Called whenever the server sends `"webrtc_ice_candidate"`.
    #[instrument(skip(self))]
    pub async fn handle_ice_candidate(
        &self,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    ) -> Result<(), String> {
        let pc_opt = self.pc.read().await;
        let pc = match &*pc_opt {
            Some(pc) => pc.clone(),
            None => {
                return Err("No PeerConnection to apply ICE candidate to".to_string());
            }
        };

        let c = RTCIceCandidateInit {
            candidate,
            sdp_mid,
            sdp_mline_index,
            ..Default::default()
        };


        let desc: Option<RTCSessionDescription> = pc.remote_description().await;
        if desc.is_none() {
            info!("Remote description is None, storing ICE candidate for later");
            self.pending_candidates.write().await.push(c);
            return Ok(()); // Delay handling until remote description is available
        }

        //info!("Adding ICE candidate to client-side PeerConnection");

        pc.add_ice_candidate(c)
            .await
            .map_err(|e| format!("Failed to add ICE candidate: {e}"))?;
        Ok(())
    }
}

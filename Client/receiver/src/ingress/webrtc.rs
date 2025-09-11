use std::{sync::{Arc, Mutex}, thread};
use rust_socketio::client::Client;
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
    /// Reference to the pipeline for decoding/storing frames
    pipeline: Arc<ProcessingPipeline>,
    /// Pending ICE candidates to be applied after the remote description is set
    pending_candidates: RwLock<Vec<RTCIceCandidateInit>>,
    pub runtime: Arc<Mutex<Option<Runtime>>>,
    track_handlers: Arc<RwLock<Vec<TrackRemotePointCloudRTP>>>,
}
crate::log_drop!(WebRTCIngress);

impl WebRTCIngress {
    /// Create a new, empty instance. Typically called once from `Ingress::initialize()`.
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        pipeline: Arc<ProcessingPipeline>,
    ) {
        let runtime = Arc::clone(&pipeline.runtime);

        let ingress = Arc::new(Self {
            pc: RwLock::new(None),
            pipeline,
            pending_candidates: RwLock::new(Vec::new()),
            runtime,
            track_handlers: Arc::new(RwLock::new(Vec::new())),
        });
        // Keep a reference to ourselves in the StreamManager
        stream_manager.set_webrtc_ingress(ingress);
    }

    pub fn stop(&self) {
        if let Some(rt) = self.runtime.lock().unwrap().as_ref() {
            rt.block_on(async {
                if let Some(pc) = self.pc.write().await.take() {
                    let _ = pc.close().await;
                } else {
                    error!("WebRTC PeerConnection was already stopped or not initialized");
                }

                let mut handlers = self.track_handlers.write().await;
                info!("Stopping {} track handlers", handlers.len());
                for mut track in handlers.drain(..) {
                    if let Err(e) = track.stop().await {
                        error!("Failed to stop track: {:?}", e);
                    } else {
                        info!("Track stopped successfully");
                    }
                }
            });
        } else {
            error!("Runtime is not available anymore, we cannot manually stop the PeerConnection");
        }
    }

    /// Actually create the PeerConnection on the client side, attach handlers, and produce an SDP offer.
    //#[instrument(skip(self))]
    pub async fn create_offer(self: Arc<Self>, ws_socket: &Arc<Mutex<Option<Client>>>) -> Result<String, String> {
        // 1) Create PeerConnection
        let pc = create_webrtc_peer_connection().await?;

        // 2) **Forward client-side ICE to server**:  
        //    Whenever the client finds a new ICE candidate,
        //    it sends it to the server as `webrtc_ice_candidate`.
        let socket_weak = Arc::downgrade(ws_socket);
        pc.on_ice_candidate(Box::new(move |c| {
            let socket_weak = socket_weak.clone();
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

                        //debug!("Client-side ICE candidate: {:?}", json_val.clone());

                        // Spawn a normal non-async thread and emit the ICE candidate
                        // Rustsocket-io doesn't support calling emit from an async context because it uses its own internal runtime.
                        // That internal runtime uses blocking, which is incompatible with our async context.
                        thread::Builder::new()
                            .name("emit-ice-candidate".to_string())
                            .spawn(move || {
                                if let Some(socket_arc) = socket_weak.upgrade() {
                                    if let Some(ref socket) = *socket_arc.lock().unwrap() {
                                        if let Err(err) = socket.emit("webrtc_ice_candidate", json_val) {
                                            error!("Failed to emit ICE candidate: {}", err);
                                        }
                                    }
                                } else {
                                    error!("WebSocket client is not connected, cannot send ICE candidate");
                                }
                            })
                            .expect("Failed to spawn ICE candidate emission thread");
                    }
                }
            })
        }));

        // Set the handler for Peer connection state
        // This will notify you when the peer has connected/disconnected
        let ingress_clone = Arc::clone(&self);
        pc.on_peer_connection_state_change(Box::new(
            move |s: RTCPeerConnectionState| {
                info!("Peer Connection State has changed: {s}");
                let ingress_clone = Arc::clone(&ingress_clone);
                Box::pin(async move {
                    if matches!(s, RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        info!("Cleaning up peer connection");
                        // Stop all tracks
                        {
                            let mut handlers = ingress_clone.track_handlers.write().await;
                            info!("Stopping {} track handlers", handlers.len());
                            for mut track in handlers.drain(..) {
                                if let Err(e) = track.stop().await {
                                    error!("Failed to stop track: {:?}", e);
                                } else {
                                    info!("Track stopped successfully");
                                }
                            }
                            info!("All tracks stopped");
                        }
                    }
                })
            })
        );

        pc.add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .map_err(|e| format!("add_transceiver_from_kind failed: {e}"))?;

        let pipeline_clone = self.pipeline.clone();
        let track_handlers_clone = Arc::clone(&self.track_handlers);
        pc.on_track(Box::new(move | track, _receiver, _transceiver| {
            let p = pipeline_clone.clone();
            let th = track_handlers_clone.clone();
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
                // Store handler so it can be stopped later
                {
                    let mut v = th.write().await;
                    let before = v.len();
                    v.push(remote_pc_track);
                    info!("on_track: handlers len {} -> {}", before, v.len());
                }
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

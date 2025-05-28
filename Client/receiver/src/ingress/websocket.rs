use std::sync::{Arc, Mutex, RwLock};
use rust_socketio::{client::Client, ClientBuilder, Payload, RawClient};
use serde_json::Value;
use tokio::runtime::Runtime;
use webrtc::ice::candidate::candidate_base::unmarshal_candidate;
use webrtc::ice::candidate::Candidate;
use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;
use shared_utils::types::FrameTaskData;
use tracing::{debug, error, info, warn};
use rbase64;

use super::{dash::DashIngress, webrtc::WebRTCIngress};

pub struct WebSocketIngress {
    url: String,
    socket: Arc<Mutex<Option<Client>>>,
    socket_id: Arc<RwLock<Option<String>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    pub runtime: Arc<Mutex<Runtime>>,
    webrtc_ingress: Arc<WebRTCIngress>,
    dash_ingress: Arc<DashIngress>,
}

impl WebSocketIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let url = stream_manager.websocket_url.read().unwrap().clone();
        if url.is_none() {
            error!("WebSocket URL is empty");
            return;
        }

        let runtime = Arc::clone(&processing_pipeline.runtime);

        // Get a reference to the WebRTCIngress
        let webrtc_ingress = {
            // The `StreamManager` now has webrtc_ingress set by WebRTCIngress::initialize
            match stream_manager.webrtc_ingress.read().unwrap().as_ref() {
                Some(i) => i.clone(),
                None => {
                    error!("WebRTCIngress not found, did you call WebRTCIngress::initialize()?");
                    return;
                }
            }
        };

        // Get a reference to the DashIngress
        let dash_ingress = {
            // The `StreamManager` now has dash_ingress set by DashIngress::initialize
            match stream_manager.dash_ingress.read().unwrap().as_ref() {
                Some(i) => i.clone(),
                None => {
                    error!("DashIngress not found, did you call DashIngress::initialize()?");
                    return;
                }
            }
        };

        let ingress = Arc::new(Self {
            url: url.unwrap(),
            socket: Arc::new(Mutex::new(None)),
            socket_id: Arc::new(RwLock::new(None)),
            processing_pipeline,
            runtime,
            webrtc_ingress,
            dash_ingress,
        });

        ingress.connect();

        stream_manager.set_websocket_ingress(ingress)
    }

    fn process_payload(
        stream_id: String,
        payload: Payload,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let Payload::Binary(bytes) = payload else {
            warn!("Unsupported payload format");
            return;
        };

        // base64 decode using rbase64
        let bytes_str = match std::str::from_utf8(&bytes) {
            Ok(v) => v,
            Err(e) => {
                warn!("Invalid UTF-8 sequence: {}", e);
                return;
            },
        };
        let bytes_decoded = match rbase64::decode(bytes_str) {
            Ok(decoded) => decoded,
            Err(err) => {
                warn!("Failed to decode payload: {}", err);
                return;
            },
        };

        // To vec
        let frame_task_data = match bitcode::decode::<FrameTaskData>(&bytes_decoded) {
            Ok(decoded) => decoded,
            Err(err) => {
                warn!("Failed to decode payload: {}", err);
                return;
            },
        };

        debug!("Received frame with {} bytes", frame_task_data.data.len());

        processing_pipeline.ingest_data(
            stream_id.clone(),
            0,
            frame_task_data.send_time,
            frame_task_data.presentation_time,
            frame_task_data.data);

    }

    pub fn get_socket(&self) -> Arc<Mutex<Option<Client>>> {
        Arc::clone(&self.socket)
    }

    pub fn get_stream_id(&self) -> String {
        // Create a stream id based on the socket id
        format!("ws_{}", self.socket_id.read().unwrap().as_deref().unwrap_or("unknown"))
    }

    pub fn connect(&self) {
        let socket_id_ref = Arc::clone(&self.socket_id);

        let socket = match ClientBuilder::new(&self.url)
            .namespace("/")
            // Some basic logging
            .on("disconnect", |_, _| info!("Disconnected from WebSocket server"))
            .on("close", |_, _| info!("Closed WebSocket connection"))
            .on("error", |err, _| error!("Error: {:#?}", err))
            // We listen for the "has_connected" event to get the socket id
            // This is a custom event that is emitted by the server to get the socket id
            // To resolve an issue with the Rust socket.io server library, we acknowledge the event. (This is not needed for other events)
            // See the comment in the server code for more information
            .on_with_ack("has_connected", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress = Arc::clone(&self.webrtc_ingress);
                let socket_id_ref = Arc::clone(&socket_id_ref);
                move |payload: Payload, s: RawClient, ack: i32| {
                    // Acknowledge the event
                    let _ = s.ack(ack, "Ok".to_string());

                    // Extract the socket id from the payload
                    let Payload::Text(values) = payload else {
                        return;
                    };

                    // The payload should contain at least 1 value: the socket id
                    if values.is_empty() {
                        return;
                    }

                    // Get the socket id
                    let socket_id = values[0].as_str().unwrap_or("").to_string();
                    info!("WebSocket connected with id: {:#?}", socket_id);

                    // Store the socket id
                    let mut socket_id_lock = socket_id_ref.write().unwrap();
                    *socket_id_lock = Some(socket_id.clone().to_string());

                    // Now that we are connected to the server, let's create our WebRTC offer.
                    // We must do this in a separate task (async).
                    let webrtc_ingress_clone = webrtc_ingress.clone();
                    let rt = runtime_clone.lock().unwrap();
                    let local_sdp = rt.block_on(webrtc_ingress_clone.create_offer());
                    match local_sdp {
                        Ok(local_sdp) => {
                            let offer_payload = serde_json::json!({
                                "sdp": local_sdp,
                                "clientId": socket_id.to_string()
                            });
                            if let Err(e) = s.emit::<&str, Value>("webrtc_offer", offer_payload) {
                                error!("Failed to emit webrtc_offer: {:?}", e);
                            }
                            //info!("Local SDP: {:#?}", local_sdp);

                        }
                        Err(e) => {
                            error!("Failed to create WebRTC offer: {}", e);
                        }
                    }
                }
            })
            .on_with_ack("webrtc_answer", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress_clone = Arc::clone(&self.webrtc_ingress);
                let socket_id_ref = Arc::clone(&socket_id_ref);
                move |payload: Payload, s: RawClient, ack: i32| {
                    let Payload::Text(values) = payload else {
                        warn!("Got webrtc_answer in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }

                    let serde_json::Value::Object(json_val) = values[0].clone() else {
                        warn!("Invalid payload format: expected an object");
                        return;
                    };

                    debug!("Received WebRTC answer from server");

                    let client_id = json_val["clientId"].as_str().unwrap_or("").to_string();
                    let socket_id_binding = socket_id_ref.read().unwrap();
                    let socket_id = socket_id_binding.as_deref().unwrap_or("unknown");
                    if client_id != socket_id {
                        warn!("Ignoring WebRTC answer: client id ({}) does not match socket id ({})", client_id, socket_id);
                        return;
                    }

                    let sdp = json_val["sdp"].as_str().unwrap_or("").to_string();
                    if sdp.is_empty() {
                        warn!("Ignoring WebRTC answer: empty SDP");
                        return;
                    }

                    let rt = runtime_clone.lock().unwrap();
                    if let Err(e) = rt.block_on(
                        webrtc_ingress_clone.handle_answer(sdp)
                    ) {
                        error!("Error handling WebRTC answer: {}", e);
                    }

                    //info!("WebRTC answer handled");

                    // Acknowledge the event
                    let _ = s.ack(ack, "Ok".to_string());
                }
            })
            .on("webrtc_ice_candidate", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress_clone = Arc::clone(&self.webrtc_ingress);
                let url = self.url.clone();
                move |payload: Payload, _s: RawClient| {
                    let Payload::Text(values) = payload else {
                        warn!("Got webrtc_ice_candidate in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }
            
                    let serde_json::Value::Object(json_val) = values[0].clone() else {
                        warn!("Invalid payload format: expected an object");
                        return;
                    };

                    //info!("Received ICE candidate: {:#?}", json_val);

                    // This is a JSON with {candidate, sdpMid, sdpMLineIndex}
                    let candidate = json_val["candidate"].as_str().unwrap_or("").to_string();
                    let sdp_mid = json_val["sdpMid"].as_str().map(|s| s.to_string());
                    let sdp_mline_index = json_val["sdpMLineIndex"].as_u64().map(|u| u as u16);

                    let candiate_clone = candidate.clone();
                    let candidate_value = match candiate_clone.strip_prefix("candidate:") {
                        Some(s) => s,
                        None => candiate_clone.as_str(),
                    };

                    let ice_candidate = if !candidate_value.is_empty() {
                        unmarshal_candidate(candidate_value)
                    } else {
                        warn!("Invalid ICE candidate: empty candidate");
                        return;
                    };

                    // If an error occurred, the ICE candidate is invalid
                    if ice_candidate.is_err() {
                        warn!("Invalid ICE candidate: {}", ice_candidate.err().unwrap());
                        return;
                    }
                    let ice_candidate = ice_candidate.unwrap();

                    // &self.url contains an url such as http://13.0.1.2:3001, extract the address
                    let mut expected_address = url.split(":").nth(1).unwrap_or("").to_string();
                    // Remove the leading "//"
                    expected_address = expected_address.strip_prefix("//").unwrap_or("").to_string();

                    let ice_address = ice_candidate.address();

                    // TODO: instead of just using the server address, we should also support a list of allowed addresses
                    if ice_address.is_empty() || ice_address != expected_address {
                        // debug!("Invalid ICE candidate: address ({}) does not match expected address ({})", ice_address, expected_address);
                        debug!("Ignoring ICE candidate: {}", ice_candidate);
                        return;
                    }

                    let rt = runtime_clone.lock().unwrap();
                    if let Err(e) = rt.block_on(
                        webrtc_ingress_clone.handle_ice_candidate(candidate, sdp_mid, sdp_mline_index)
                    ) {
                        error!("Error handling ICE candidate: {}", e);
                    }

                    debug!("ICE candidate handled");
                }
            })
            .on_with_ack("frame:broadcast:ack", {
                let stream_id = self.get_stream_id();
                let processing_pipeline = Arc::clone(&self.processing_pipeline);
                move |payload: Payload, s: RawClient, ack: i32| {
                    let _ = s.ack(ack, "Ok".to_string());
                    debug!("Received frame broadcast with ack");
                    WebSocketIngress::process_payload(stream_id.clone(), payload, Arc::clone(&processing_pipeline));
                }
            })
            .on("frame:broadcast", {
                let stream_id = self.get_stream_id();
                let processing_pipeline = Arc::clone(&self.processing_pipeline);
                move |payload: Payload, _s: RawClient| {
                    debug!("Received frame broadcast without ack");
                    WebSocketIngress::process_payload(stream_id.clone(), payload, Arc::clone(&processing_pipeline));
                }
            })
            .on("mpd::group_id",{
                let dash_ingress = Arc::clone(&self.dash_ingress);
                move |payload: Payload, _s: RawClient| {
                    let Payload::Text(values) = payload else {
                        warn!("Got mpd::group_id in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }

                    let group_id = values[0].as_str().unwrap_or("").to_string();
                    info!("Received MPD group id: {:#?}", group_id);

                    dash_ingress.spawn_group(group_id.clone());
                }
            })
            .connect() {
                Ok(s) => s,
                Err(err) => {
                    error!("Failed to connect WebSocket: {:#?}", err);
                    return;
                }
            };
        
        
        // Store the socket
        let mut socket_lock = self.socket.lock().unwrap();
        *socket_lock = Some(socket);

    }
}

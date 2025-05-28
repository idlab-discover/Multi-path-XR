use std::{collections::HashMap, sync::Arc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

pub type ActiveJobs = Arc<tokio::sync::RwLock<HashMap<String, oneshot::Sender<()>>>>;


#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct FOV {
    // Define FOV parameters (e.g., position, orientation, angle)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EgressProtocolType {
    WebSocket,
    WebRTC,
    Flute,
    File,
    Buffer,
    // Add other egress protocols as needed
}

#[derive(Clone, Debug)]
pub struct StreamSettings {
    pub stream_id: String,
    pub priority: u8,
    pub egress_protocols: Vec<EgressProtocolType>,
    pub process_incoming_frames: bool,
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    pub presentation_time_offset: Option<u64>,
    pub sfu_client_id: Option<u64>,
    pub sfu_tile_index: Option<u32>,

    // Toggles for bypassing processing stages
    // These could reduce latency but will limit the functionality
    // Additionally, these are not safe against congestion in the pipeline.
    pub decode_bypass: bool, // Instead of decoding, we treat the data as “the final data” to pass on.
    pub aggregator_bypass: bool,
    pub ring_buffer_bypass: bool, // Emit directly to the egress protocol without buffering. This is not safe against congestion in the pipeline.

    // Optionally, we can make our egress emit one incomming frame as multiple partial frames.
    // This is useful for Multiple Description Coding (MDC)
    // We could also give priority to certain partial frames such that at least some of them are being received.
    pub max_point_percentages: Option<Vec<u8>>,   // e.g. [15, 25, 60]
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub stream_manager: Arc<crate::services::stream_manager::StreamManager>,
    pub processing_pipeline: Arc<crate::processing::ProcessingPipeline>,
    pub active_jobs: Arc<ActiveJobs>,
    pub socket_io: Arc<socketioxide::SocketIo>,
}

/// Event used for containing SDP data and the room ID.
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WebRtcOffer {
    pub sdp: String,
    #[serde(rename = "clientId")]
    pub client_id: String,
}

/// Event used for containing ICE candidate data and the room ID.
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WebRtcIceCandidate {
    pub candidate: String,
    #[serde(rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct WebRtcIceCandidateResponse {
    pub candidate: Value,
}
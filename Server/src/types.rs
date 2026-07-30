use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

pub type ActiveJobs = Arc<tokio::sync::RwLock<HashMap<String, oneshot::Sender<()>>>>;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Fov {
    // Define FOV parameters (e.g., position, orientation, angle, far/near planes (depth))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EgressProtocolType {
    WebSocket,
    WebRTC,
    Flute,
    File,
    Buffer,
    Moq,
    // Add other egress protocols as needed
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamPayloadFormat {
    Auto,
    DecodedPoints,
    DecodedGaussianSplats,
    PreencodedPoints,
    PreencodedGaussianSplats,
}

impl StreamPayloadFormat {
    #[inline]
    pub fn is_preencoded(self) -> bool {
        matches!(
            self,
            Self::PreencodedPoints | Self::PreencodedGaussianSplats
        )
    }
}

impl Default for StreamPayloadFormat {
    fn default() -> Self {
        Self::Auto
    }
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
    pub client_id: Option<u64>,
    pub quality_index: Option<u32>,
    pub input_format: StreamPayloadFormat,

    // Toggles for bypassing processing stages
    // These could reduce latency but will limit the functionality
    // Additionally, these are not safe against congestion in the pipeline.
    pub decode_bypass: bool, // Instead of decoding, we treat the data as “the final data” to pass on.
    pub aggregator_bypass: bool,
    pub ring_buffer_bypass: bool, // Emit directly to the egress protocol without buffering. This is not safe against congestion in the pipeline.

    // Optionally, we can make our egress emit one incomming frame as multiple partial frames.
    // This is useful for Multiple Description Coding (MDC)
    // We could also give priority to certain partial frames such that at least some of them are being received.
    pub max_primitive_percentages: Option<Vec<u8>>, // e.g. [15, 25, 60]
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub stream_manager: Arc<crate::services::stream_manager::StreamManager>,
    pub processing_pipeline: Arc<crate::processing::ProcessingPipeline>,
    pub active_jobs: Arc<ActiveJobs>,
    pub socket_io: Arc<socketioxide::SocketIo>,
    pub moq_config: Option<AdvertisedMoqConfig>,
    pub moq_registry: Arc<MoqRelayRegistry>,
    pub server_instance_id: Arc<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdvertisedMoqConfig {
    pub url: String,
    pub namespace: String,
    pub tls_ca_pem: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MoqRelayStatus {
    pub relay_url: String,
    pub namespace: String,
    pub announce_url: Option<String>,
    pub last_update_ms: i64,
}

#[derive(Debug, Default)]
pub struct MoqRelayRegistry {
    relays: Mutex<HashMap<String, MoqRelayStatus>>,
}

impl MoqRelayRegistry {
    // TODO: use dashmap for better performance if needed
    pub fn update(&self, status: MoqRelayStatus) {
        self.relays
            .lock()
            .unwrap()
            .insert(status.relay_url.clone(), status);
    }

    pub fn snapshot(&self) -> Vec<MoqRelayStatus> {
        self.relays.lock().unwrap().values().cloned().collect()
    }
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockSyncRequest {
    pub local_send_us: u64,
    pub sequence: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockSyncResponse {
    pub server_instance_id: String,
    pub local_send_us: u64,
    pub remote_receive_us: u64,
    pub remote_send_us: u64,
    pub sequence: Option<u64>,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct WebRtcIceCandidateResponse {
    pub candidate: Value,
}

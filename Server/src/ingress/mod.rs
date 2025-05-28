pub mod webrtc;
pub mod websocket;
// Add other ingress protocols as needed

use std::sync::Arc;
use tracing::instrument;

use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;

#[instrument(skip_all)]
pub fn initialize_ingress_protocols(
    stream_manager: Arc<StreamManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
) {
    webrtc::WebRTCIngress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    websocket::WebSocketIngress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    // Initialize other ingress protocols similarly
}

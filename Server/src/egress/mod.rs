// egress/mod.rs

use std::sync::Arc;
use tracing::instrument;

use crate::processing::ProcessingPipeline;
use crate::services::mpd_manager::MpdManager;
use crate::services::stream_manager::StreamManager;

pub mod buffer;
pub mod egress_common;
pub mod file;
pub mod flute;
pub mod moq;
pub mod webrtc;
pub mod websocket;
// Add other egress protocols as needed

#[instrument(skip_all)]
pub fn initialize_egress_protocols(
    stream_manager: Arc<StreamManager>,
    mpd_manager: Arc<MpdManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    flute_endpoint_url: String,
    flute_port: u16,
    moq_config: Option<moq::MoqEgressConfig>,
    server_instance_id: Arc<String>,
) {
    webrtc::WebRTCEgress::initialize(stream_manager.clone(), processing_pipeline.clone());

    websocket::WebSocketEgress::initialize(stream_manager.clone(), processing_pipeline.clone());

    flute::FluteEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
        flute_endpoint_url,
        flute_port,
        server_instance_id.clone(),
    );

    file::FileEgress::initialize(stream_manager.clone(), processing_pipeline.clone());

    buffer::BufferEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
        mpd_manager.clone(),
    );

    if let Some(config) = moq_config {
        moq::MoqEgress::initialize(
            stream_manager.clone(),
            processing_pipeline.clone(),
            config,
            server_instance_id.clone(),
        );
    } else {
        tracing::info!("MoQ egress not configured; skipping initialization");
    }
}

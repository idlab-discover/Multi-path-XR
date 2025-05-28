// egress/mod.rs

use std::sync::Arc;
use tracing::instrument;

use crate::services::mpd_manager::MpdManager;
use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;

pub mod egress_common;
pub mod flute;
pub mod webrtc;
pub mod websocket;
pub mod file;
pub mod buffer;
// Add other egress protocols as needed

#[instrument(skip_all)]
pub fn initialize_egress_protocols(
    stream_manager: Arc<StreamManager>,
    mpd_manager: Arc<MpdManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    flute_endpoint_url: String,
    flute_port: u16,
) {
    webrtc::WebRTCEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    websocket::WebSocketEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    flute::FluteEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
        flute_endpoint_url,
        flute_port,
    );

    file::FileEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    buffer::BufferEgress::initialize(
        stream_manager.clone(),
        processing_pipeline.clone(),
        mpd_manager.clone(),
    );

}

pub mod websocket;
pub mod webrtc;
pub mod flute;
pub mod dash;
// pub mod flute; // Implement when ready
use std::sync::Arc;
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use crate::storage::Storage;

pub struct Ingress {
    stream_manager: Arc<StreamManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    storage: Arc<Storage>,
}

impl Ingress {
    pub fn new(thread_count: usize, disable_parser: bool) -> Self {
        let stream_manager = Arc::new(StreamManager::new());
        let storage = Arc::new(Storage::new());
        let processing_pipeline = Arc::new(ProcessingPipeline::new(storage.clone(), thread_count, disable_parser));
        Ingress {
            stream_manager,
            processing_pipeline,
            storage,
        }
    }

    pub fn initialize(&self) {
    
        webrtc::WebRTCIngress::initialize(
            self.stream_manager.clone(),
            self.processing_pipeline.clone(),
        );

        dash::DashIngress::initialize(
            self.stream_manager.clone(),
            self.processing_pipeline.clone(),
        );

        websocket::WebSocketIngress::initialize(
            self.stream_manager.clone(),
            self.processing_pipeline.clone(),
        );

        flute::FluteIngress::initialize(
            self.stream_manager.clone(),
            self.processing_pipeline.clone(),
        );
    }

    pub fn get_stream_manager(&self) -> Arc<StreamManager> {
        self.stream_manager.clone()
    }

    pub fn get_storage(&self) -> Arc<Storage> {
        self.storage.clone()
    }
}

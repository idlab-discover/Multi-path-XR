use std::fmt;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use tracing::instrument;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;


pub struct WebRTCIngress {
    data_channels: Arc<RwLock<HashMap<String, Arc<RTCDataChannel>>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
}

impl fmt::Debug for WebRTCIngress {
    #[instrument(skip_all)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebRTCIngress")
            .field("data_channels", &"<hidden>") // Replace with something meaningful or omit
            .field("processing_pipeline", &self.processing_pipeline)
            .field("stream_manager", &self.stream_manager)
            .finish()
    }
}

impl WebRTCIngress {
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let instance = Arc::new(Self {
            data_channels: Arc::new(RwLock::new(HashMap::new())),
            processing_pipeline,
            stream_manager: stream_manager.clone(),
        });

        // Store the instance in the StreamManager
        stream_manager.set_webrtc_ingress(instance.clone());
    }

    #[instrument(skip_all)]
    pub fn add_data_channel(&self, stream_id: String, data_channel: Arc<RTCDataChannel>) {
        self.data_channels.write().unwrap().insert(stream_id.clone(), data_channel.clone());

        let processing_pipeline = self.processing_pipeline.clone();
        let stream_manager = self.stream_manager.clone();
        let stream_id_clone = stream_id.clone();
        data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let stream_id_clone = stream_id_clone.clone();
            let stream_manager_clone = stream_manager.clone();
            let processing_pipeline_clone = processing_pipeline.clone();
            Box::pin(async move {
                processing_pipeline_clone.push_to_decoder(msg.data.to_vec(), stream_manager_clone, stream_id_clone);
            })
        }));

        // Automatic removal is handled in the webrtc egress and in the websocket handler
    }

    #[instrument(skip_all)]
    pub fn remove_data_channel(&self, stream_id: &str) {
        self.data_channels.write().unwrap().remove(stream_id);
    }

}

use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use socketioxide::extract::{Data, SocketRef};
use tracing::instrument;
use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;

#[derive(Debug)]
pub struct WebSocketIngress {
    sockets: RwLock<HashMap<String, Arc<SocketRef>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    stream_manager: Arc<StreamManager>,
}

impl WebSocketIngress {
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let instance = Arc::new(Self {
            sockets: RwLock::new(HashMap::new()),
            processing_pipeline,
            stream_manager: stream_manager.clone(),
        });

        // Store the instance in the StreamManager
        stream_manager.set_websocket_ingress(instance.clone());
    }

    #[instrument(skip_all)]
    pub fn add_socket(&self, stream_id: String, socket: Arc<SocketRef>) {
        self.sockets.write().unwrap().insert(stream_id.clone(), socket.clone());

        let processing_pipeline = self.processing_pipeline.clone();
        let stream_manager = self.stream_manager.clone();
        socket.on("frame", move |_s: SocketRef, Data(data): Data<Vec<u8>>| {
            let stream_id_clone = stream_id.clone();
            let stream_manager_clone = stream_manager.clone();
            let processing_pipeline_clone = processing_pipeline.clone();

            processing_pipeline_clone.push_to_decoder(data.clone(), stream_manager_clone, stream_id_clone);
        });


    }
}

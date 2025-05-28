use socketioxide::SocketIo;
use tracing::instrument;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use crate::egress::buffer::BufferEgress;
use crate::egress::egress_common::EgressProtocol;
use crate::egress::file::FileEgress;
use crate::egress::flute::FluteEgress;
use crate::egress::webrtc::WebRTCEgress;
use crate::egress::websocket::WebSocketEgress;
use crate::ingress::webrtc::WebRTCIngress;
use crate::ingress::websocket::WebSocketIngress;
use crate::types::{StreamSettings, EgressProtocolType};

#[derive(Debug)]
pub struct StreamManager {
    // Map of stream_id to StreamSettings
    pub stream_settings: RwLock<HashMap<String, StreamSettings>>,
    // Reference to the socket.io instance
    pub socket_io: RwLock<Option<Arc<SocketIo>>>,
    // References to singleton egress protocols
    pub webrtc_egress: RwLock<Option<Arc<WebRTCEgress>>>,
    pub websocket_egress: RwLock<Option<Arc<WebSocketEgress>>>,
    pub flute_egress: RwLock<Option<Arc<FluteEgress>>>,
    pub file_egress: RwLock<Option<Arc<FileEgress>>>,
    pub buffer_egress: RwLock<Option<Arc<BufferEgress>>>,
    // Ingress protocol singletons
    pub webrtc_ingress: RwLock<Option<Arc<WebRTCIngress>>>,
    pub websocket_ingress: RwLock<Option<Arc<WebSocketIngress>>>,
}

impl StreamManager {
    #[instrument(skip_all)]
    pub fn new() -> Self {
        Self {
            socket_io: RwLock::new(None),
            webrtc_egress: RwLock::new(None),
            websocket_egress: RwLock::new(None),
            flute_egress: RwLock::new(None),
            file_egress: RwLock::new(None),
            buffer_egress: RwLock::new(None),
            stream_settings: RwLock::new(HashMap::new()),
            webrtc_ingress: RwLock::new(None),
            websocket_ingress: RwLock::new(None),
        }
    }

    #[instrument(skip_all)]
    pub fn get_stream_settings(&self, stream_id: &str) -> StreamSettings {
        let read_guard = self.stream_settings.read().unwrap();
        if let Some(settings) = read_guard.get(stream_id).cloned() {
            return settings;
        }
        drop(read_guard); // Release the read lock before acquiring a write lock

        let mut write_guard = self.stream_settings.write().unwrap();
        let mut settings = if write_guard.contains_key("__default__") {
            let mut default = write_guard.get("__default__").cloned().unwrap();
            default.stream_id = stream_id.to_owned();
            default
        } else {
            StreamSettings {
                stream_id: stream_id.to_owned(),
                priority: 0,
                egress_protocols: vec![EgressProtocolType::WebSocket],
                process_incoming_frames: true,
                position: [0.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                presentation_time_offset: None,
                decode_bypass: false,
                aggregator_bypass: false,
                ring_buffer_bypass: false,
                sfu_client_id: None,
                sfu_tile_index: None,
                max_point_percentages: None,
            }
        };

        // Try to extract SFU client ID and tile index from stream_id
        // This is a workaround for the SFU client ID and tile index being part of the stream_id
        if settings.stream_id.starts_with("client_") {
            let parts = settings.stream_id.split('_').collect::<Vec<_>>();
            if parts.len() > 2 {
                settings.sfu_client_id = parts[1].parse::<u64>().ok();
                settings.sfu_tile_index = parts[2].parse::<u32>().ok();
            }
        }

        write_guard.insert(stream_id.to_owned(), settings.clone());
        settings
    }

    #[instrument(skip_all)]
    pub fn update_stream_settings(&self, settings: StreamSettings) {
        self.stream_settings.write().unwrap().insert(settings.stream_id.clone(), settings);
    }


    #[instrument(skip_all)]
    pub fn set_socket_io(&self, socket_io: Arc<SocketIo>) {
        *self.socket_io.write().unwrap() = Some(socket_io);
    }

    #[instrument(skip_all)]
    pub fn get_socket_io(&self) -> Option<Arc<SocketIo>> {
        self.socket_io.read().unwrap().clone()
    }

    // Methods to set and get egress protocol singletons
    pub fn get_egress(
        &self,
        kind: &EgressProtocolType,
    ) -> Option<Arc<dyn EgressProtocol>> {
        use EgressProtocolType::*;
        match kind {
            WebSocket => self.get_websocket_egress().map(|e| e as _),
            WebRTC    => self.get_webrtc_egress   ().map(|e| e as _),
            Flute     => self.get_flute_egress    ().map(|e| e as _),
            File      => self.get_file_egress     ().map(|e| e as _),
            Buffer    => self.get_buffer_egress   ().map(|e| e as _),
        }
    }

    pub fn get_egresses(
        &self,
        kinds: &[EgressProtocolType],
    ) -> Vec<Arc<dyn EgressProtocol>> {
        kinds.iter()
            .filter_map(|kind| self.get_egress(kind))
            .collect()
    }


    #[instrument(skip_all)]
    pub fn set_webrtc_egress(&self, egress: Arc<WebRTCEgress>) {
        *self.webrtc_egress.write().unwrap() = Some(egress);
    }

    #[instrument(skip_all)]
    pub fn get_webrtc_egress(&self) -> Option<Arc<WebRTCEgress>> {
        self.webrtc_egress.read().unwrap().clone()
    }

    #[instrument(skip_all)]
    pub fn set_websocket_egress(&self, egress: Arc<WebSocketEgress>) {
        *self.websocket_egress.write().unwrap() = Some(egress);
    }

    #[instrument(skip_all)]
    pub fn get_websocket_egress(&self) -> Option<Arc<WebSocketEgress>> {
        self.websocket_egress.read().unwrap().clone()
    }

    #[instrument(skip_all)]
    pub fn set_flute_egress(&self, egress: Arc<FluteEgress>) {
        *self.flute_egress.write().unwrap() = Some(egress);
    }
    
    #[instrument(skip_all)]
    pub fn get_flute_egress(&self) -> Option<Arc<FluteEgress>> {
        self.flute_egress.read().unwrap().clone()
    }

    #[instrument(skip_all)]
    pub fn set_file_egress(&self, egress: Arc<FileEgress>) {
        *self.file_egress.write().unwrap() = Some(egress);
    }
    
    #[instrument(skip_all)]
    pub fn get_file_egress(&self) -> Option<Arc<FileEgress>> {
        self.file_egress.read().unwrap().clone()
    }

    #[instrument(skip_all)]
    pub fn set_buffer_egress(&self, egress: Arc<BufferEgress>) {
        *self.buffer_egress.write().unwrap() = Some(egress);
    }
    
    #[instrument(skip_all)]
    pub fn get_buffer_egress(&self) -> Option<Arc<BufferEgress>> {
        self.buffer_egress.read().unwrap().clone()
    }

    // Methods to set and get ingress protocol singletons
    #[instrument(skip_all)]
    pub fn set_webrtc_ingress(&self, ingress: Arc<crate::ingress::webrtc::WebRTCIngress>) {
        *self.webrtc_ingress.write().unwrap() = Some(ingress);
    }

    #[instrument(skip_all)]
    pub fn get_webrtc_ingress(&self) -> Option<Arc<crate::ingress::webrtc::WebRTCIngress>> {
        self.webrtc_ingress.read().unwrap().clone()
    }

    pub fn set_websocket_ingress(&self, ingress: Arc<crate::ingress::websocket::WebSocketIngress>) {
        *self.websocket_ingress.write().unwrap() = Some(ingress);
    }

    #[instrument(skip_all)]
    pub fn get_websocket_ingress(&self) -> Option<Arc<crate::ingress::websocket::WebSocketIngress>> {
        self.websocket_ingress.read().unwrap().clone()
    }

    // Existing methods for managing sockets...
}

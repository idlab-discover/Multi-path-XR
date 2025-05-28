use std::sync::{Arc, RwLock};
use crate::ingress::dash::DashIngress;
use crate::ingress::flute::FluteIngress;
use crate::ingress::websocket::WebSocketIngress;
use crate::ingress::webrtc::WebRTCIngress;

pub struct StreamManager {
    pub websocket_ingress: RwLock<Option<Arc<WebSocketIngress>>>,
    pub webrtc_ingress: RwLock<Option<Arc<WebRTCIngress>>>,
    pub dash_ingress: RwLock<Option<Arc<DashIngress>>>,
    pub flute_ingress: RwLock<Option<Arc<FluteIngress>>>,
    pub websocket_url: RwLock<Option<String>>,
    pub flute_url: RwLock<Option<String>>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            websocket_ingress: RwLock::new(None),
            webrtc_ingress: RwLock::new(None),
            dash_ingress: RwLock::new(None),
            flute_ingress: RwLock::new(None),
            websocket_url: RwLock::new(None),
            flute_url: RwLock::new(None),
        }
    }

    pub fn set_websocket_ingress(&self, ingress: Arc<WebSocketIngress>) {
        *self.websocket_ingress.write().unwrap() = Some(ingress);
    }

    pub fn set_webrtc_ingress(&self, ingress: Arc<WebRTCIngress>) {
        *self.webrtc_ingress.write().unwrap() = Some(ingress);
    }

    pub fn set_dash_ingress(&self, ingress: Arc<DashIngress>) {
        *self.dash_ingress.write().unwrap() = Some(ingress);
    }

    pub fn set_flute_ingress(&self, ingress: Arc<FluteIngress>) {
        *self.flute_ingress.write().unwrap() = Some(ingress);
    }

    pub fn set_websocket_url(&self, url: String) {
        *self.websocket_url.write().unwrap() = Some(url);
    }

    pub fn set_flute_url(&self, url: String) {
        *self.flute_url.write().unwrap() = Some(url);
    }
}

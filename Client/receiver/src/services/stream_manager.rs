use abr_core::{AbrMode, AbrModeHandle};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::info;

use crate::ingress::dash::DashIngress;
use crate::ingress::flute::FluteIngress;
use crate::ingress::moq::MoqIngress;
use crate::ingress::webrtc::WebRTCIngress;
use crate::ingress::websocket::WebSocketIngress;
use url::Url;

pub struct StreamManager {
    pub websocket_ingress: RwLock<Option<Arc<WebSocketIngress>>>,
    pub webrtc_ingress: RwLock<Option<Arc<WebRTCIngress>>>,
    pub dash_ingress: RwLock<Option<Arc<DashIngress>>>,
    pub flute_ingress: RwLock<Option<Arc<FluteIngress>>>,
    pub moq_ingress: RwLock<Option<Arc<MoqIngress>>>,
    pub http_url: RwLock<Option<String>>,
    pub websocket_url: RwLock<Option<String>>,
    pub flute_url: RwLock<Option<String>>,
    pub moq_config: RwLock<Option<MoqClientConfig>>,
    abr_mode: AbrModeHandle,
}
crate::log_drop!(StreamManager);

#[derive(Clone)]
pub struct MoqClientConfig {
    pub url: Url,
    pub namespace: String,
    pub bind: SocketAddr,
    pub tls: MoqTlsConfig,
}

#[derive(Clone, Default)]
pub struct MoqTlsConfig {
    pub cert: Vec<PathBuf>,
    pub key: Vec<PathBuf>,
    pub root: Vec<PathBuf>,
    pub disable_verify: bool,
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamManager {
    pub fn new() -> Self {
        Self {
            websocket_ingress: RwLock::new(None),
            webrtc_ingress: RwLock::new(None),
            dash_ingress: RwLock::new(None),
            flute_ingress: RwLock::new(None),
            moq_ingress: RwLock::new(None),
            http_url: RwLock::new(None),
            websocket_url: RwLock::new(None),
            flute_url: RwLock::new(None),
            moq_config: RwLock::new(None),
            abr_mode: AbrModeHandle::new(AbrMode::Simple),
        }
    }

    pub fn stop(&self) {
        if let Some(ingress) = self.websocket_ingress.write().unwrap().take() {
            ingress.stop();
            info!("WebSocket ingress stopped");
        }
        if let Some(ingress) = self.webrtc_ingress.write().unwrap().take() {
            ingress.stop();
            info!("WebRTC ingress stopped");
        }
        if let Some(ingress) = self.dash_ingress.write().unwrap().take() {
            ingress.stop();
            info!("DASH ingress stopped");
        }
        if let Some(ingress) = self.flute_ingress.write().unwrap().take() {
            ingress.stop();
            info!("Flute ingress stopped");
        }
        if let Some(ingress) = self.moq_ingress.write().unwrap().take() {
            ingress.stop();
            info!("MoQ ingress stopped");
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

    pub fn set_moq_ingress(&self, ingress: Arc<MoqIngress>) {
        *self.moq_ingress.write().unwrap() = Some(ingress);
    }

    pub fn set_http_url(&self, url: String) {
        *self.http_url.write().unwrap() = Some(url);
    }

    pub fn set_websocket_url(&self, url: String) {
        *self.websocket_url.write().unwrap() = Some(url);
    }

    pub fn set_flute_url(&self, url: String) {
        *self.flute_url.write().unwrap() = Some(url);
    }

    pub fn set_moq_config(&self, config: MoqClientConfig) {
        *self.moq_config.write().unwrap() = Some(config);
    }

    pub fn take_moq_config(&self) -> Option<MoqClientConfig> {
        self.moq_config.write().unwrap().take()
    }

    pub fn abr_mode(&self) -> AbrMode {
        self.abr_mode.get()
    }

    pub fn abr_mode_handle(&self) -> AbrModeHandle {
        self.abr_mode.clone()
    }

    pub fn set_abr_mode(&self, mode: AbrMode) {
        self.abr_mode.set(mode);
    }
}

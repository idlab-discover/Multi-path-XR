use std::sync::Arc;

use tracing::{debug, info};

use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;

/// Compatibility-only MoQ ingress.
///
/// The public API and configuration surface are retained, but the unpublished
/// subscriber and transport implementation are not part of this build.
pub struct MoqIngress {
    relay_url: String,
    namespace: String,
}
crate::log_drop!(MoqIngress);

impl MoqIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        _processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let Some(config) = stream_manager.take_moq_config() else {
            debug!("MoQ ingress not configured; skipping compatibility initialization");
            return;
        };

        let ingress = Arc::new(Self {
            relay_url: config.url.to_string(),
            namespace: config.namespace,
        });
        info!(
            "MoQ compatibility ingress configured for {} in namespace {}; networking is unavailable in this build",
            ingress.relay_url, ingress.namespace
        );
        stream_manager.set_moq_ingress(ingress);
    }

    pub fn stop(&self) {}
}

use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use tracing::info;
use url::Url;

use super::egress_common::{AtomicEncodingFormat, EgressProtocol};
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use shared_utils::types::{FramePayloadMetadata, FrameTaskData, SpatialFrameData};
use spatial_codecs::encoder::EncodingFormat;

/// Public MoQ configuration surface retained for API compatibility.
pub struct MoqEgressConfig {
    pub url: Url,
    pub namespace: String,
}

/// Compatibility-only MoQ egress.
///
/// This public build retains the configuration and control surface but does not
/// contain the unpublished MoQ transport implementation.
pub struct MoqEgress {
    relay_url: String,
    namespace: String,
    fps: AtomicU32,
    encoding_format: AtomicEncodingFormat,
    max_number_of_primitives: AtomicU64,
}

impl fmt::Debug for MoqEgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoqEgress")
            .field("relay_url", &self.relay_url)
            .field("namespace", &self.namespace)
            .field("networking_available", &false)
            .finish()
    }
}

impl MoqEgress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        _processing_pipeline: Arc<ProcessingPipeline>,
        config: MoqEgressConfig,
        _server_instance_id: Arc<String>,
    ) {
        let instance = Arc::new(Self {
            relay_url: config.url.to_string(),
            namespace: config.namespace,
            fps: AtomicU32::new(30),
            encoding_format: AtomicEncodingFormat::new(EncodingFormat::Draco),
            max_number_of_primitives: AtomicU64::new(100_000),
        });

        info!(
            "MoQ compatibility egress configured for {}; networking is unavailable in this build",
            instance.relay_url
        );
        stream_manager.set_moq_egress(instance);
    }
}

impl EgressProtocol for MoqEgress {
    fn encoding_format(&self) -> EncodingFormat {
        self.encoding_format.load()
    }

    fn max_number_of_primitives(&self) -> u64 {
        self.max_number_of_primitives.load(Ordering::Relaxed)
    }

    fn ensure_threads_started(&self) {}

    fn push_spatial_frame(&self, _spatial_frame: SpatialFrameData, _stream_id: String) {}

    fn push_encoded_frame(
        &self,
        _raw_data: Vec<u8>,
        _stream_id: String,
        _creation_time: u64,
        _presentation_time: u64,
        _ring_buffer_bypass: bool,
        _payload_metadata: Option<FramePayloadMetadata>,
        _client_id: Option<u64>,
        _quality_index: Option<u32>,
    ) {
    }

    fn emit_frame_data(&self, _frame: FrameTaskData) {}

    fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::Relaxed);
    }

    fn set_encoding_format(&self, encoding_format: EncodingFormat) {
        self.encoding_format.store(encoding_format);
    }

    fn set_max_number_of_primitives(&self, max_number_of_primitives: u64) {
        self.max_number_of_primitives
            .store(max_number_of_primitives, Ordering::Relaxed);
    }
}

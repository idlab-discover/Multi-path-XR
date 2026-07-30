use crate::egress::egress_common::EgressProtocol;
use crate::types::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use spatial_codecs::encoder::EncodingFormat;
use tracing::{info, instrument, warn};

#[derive(Deserialize, Debug)]
pub struct UpdateEgressSettingsRequest {
    // Common settings
    pub fps: Option<u32>,
    pub encoding_format: Option<EncodingFormat>,
    pub max_number_of_primitives: Option<u64>,
    // Legacy query parameter used by archived experiment YAMLs.
    pub max_number_of_points: Option<u64>,
    // WebSocket-specific settings
    pub emit_with_ack: Option<bool>,
    // FLUTE-specific settings
    pub content_encoding: Option<String>,
    pub fec: Option<String>,
    pub fec_percentage: Option<f32>,
    pub bandwidth: Option<u32>,
    pub md5: Option<bool>,
    // Target egress protocol
    pub egress_protocol: String, // "WebSocket", "WebRTC or "FLUTE"
}

impl UpdateEgressSettingsRequest {
    fn max_number_of_primitives(&self) -> Option<u64> {
        self.max_number_of_primitives.or(self.max_number_of_points)
    }
}

#[derive(Serialize, Debug)]
pub struct UpdateEgressSettingsResponse {
    pub message: String,
}

#[instrument(skip_all)]
pub async fn update_egress_settings(
    Query(params): Query<UpdateEgressSettingsRequest>,
    State(state): State<AppState>,
) -> Json<UpdateEgressSettingsResponse> {
    let egress_protocol = params.egress_protocol.to_lowercase();

    match egress_protocol.as_str() {
        "websocket" => {
            if let Some(websocket_egress) = state.stream_manager.get_websocket_egress() {
                // Arc<WebSocketEgress>
                // Update FPS
                if let Some(fps) = params.fps {
                    websocket_egress.set_fps(fps);
                    info!("WebSocketEgress FPS updated to {}", fps);
                }
                // Update encoding format
                if let Some(encoding_format) = params.encoding_format {
                    websocket_egress.set_encoding_format(encoding_format);
                    info!(
                        "WebSocketEgress encoding format updated to {:?}",
                        encoding_format
                    );
                }
                // Update max number of primitives
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    websocket_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "WebSocketEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }
                // Update emit_with_ack
                if let Some(emit_with_ack) = params.emit_with_ack {
                    websocket_egress.set_emit_with_ack(emit_with_ack);
                    info!("WebSocketEgress emit_with_ack updated to {}", emit_with_ack);
                }

                Json(UpdateEgressSettingsResponse {
                    message: "WebSocketEgress settings updated".to_string(),
                })
            } else {
                warn!("WebSocketEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "WebSocketEgress not initialized".to_string(),
                })
            }
        }
        "webrtc" => {
            if let Some(webrtc_egress) = state.stream_manager.get_webrtc_egress() {
                // Update FPS
                if let Some(fps) = params.fps {
                    webrtc_egress.set_fps(fps);
                    info!("WebRTCEgress FPS updated to {}", fps);
                }
                // Update encoding format
                if let Some(encoding_format) = params.encoding_format {
                    webrtc_egress.set_encoding_format(encoding_format);
                    info!(
                        "WebRTCEgress encoding format updated to {:?}",
                        encoding_format
                    );
                }
                // Update max number of primitives
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    webrtc_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "WebRTCEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }

                Json(UpdateEgressSettingsResponse {
                    message: "WebRTCEgress settings updated".to_string(),
                })
            } else {
                warn!("WebRTCEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "WebRTCEgress not initialized".to_string(),
                })
            }
        }
        "flute" => {
            if let Some(flute_egress) = state.stream_manager.get_flute_egress() {
                // Update FPS
                if let Some(fps) = params.fps {
                    flute_egress.set_fps(fps);
                    info!("FluteEgress FPS updated to {}", fps);
                }
                // Update encoding format
                if let Some(encoding_format) = params.encoding_format {
                    flute_egress.set_encoding_format(encoding_format);
                    info!(
                        "FluteEgress encoding format updated to {:?}",
                        encoding_format
                    );
                }
                // Update max number of primitives
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    flute_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "FluteEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }

                // Update the content encoding
                if let Some(content_encoding) = params.content_encoding {
                    flute_egress.set_content_encoding(content_encoding.clone());
                    info!(
                        "FluteEgress content encoding updated to {}",
                        content_encoding
                    );
                }

                if let Some(bandwidth) = params.bandwidth {
                    flute_egress.set_bandwidth(bandwidth);
                    info!("FluteEgress bandwidth updated to {}", bandwidth);
                }

                if let Some(md5) = params.md5 {
                    flute_egress.set_md5(md5);
                    info!("FluteEgress md5 updated to {}", md5);
                }

                let mut should_destroy_sender = false;

                if let Some(fec) = params.fec {
                    flute_egress.set_fec(fec.clone());
                    info!("FluteEgress FEC updated to {}", fec);
                    should_destroy_sender = true;
                }

                if let Some(fec_percentage) = params.fec_percentage {
                    flute_egress.set_fec_parity_percentage(fec_percentage);
                    info!("FluteEgress FEC percentage updated to {}", fec_percentage);
                    should_destroy_sender = true;
                }

                if should_destroy_sender {
                    flute_egress.destroy_sender();
                }

                Json(UpdateEgressSettingsResponse {
                    message: "FluteEgress settings updated".to_string(),
                })
            } else {
                warn!("FluteEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "FluteEgress not initialized".to_string(),
                })
            }
        }
        "file" => {
            if let Some(file_egress) = state.stream_manager.get_file_egress() {
                // Arc<FileEgress>
                // Update FPS
                if let Some(fps) = params.fps {
                    file_egress.set_fps(fps);
                    info!("FileEgress FPS updated to {}", fps);
                }
                // Update encoding format
                if let Some(encoding_format) = params.encoding_format {
                    file_egress.set_encoding_format(encoding_format);
                    info!(
                        "FileEgress encoding format updated to {:?}",
                        encoding_format
                    );
                }
                // Update max number of primitives
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    file_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "FileEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }

                Json(UpdateEgressSettingsResponse {
                    message: "FileEgress settings updated".to_string(),
                })
            } else {
                warn!("FileEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "FileEgress not initialized".to_string(),
                })
            }
        }
        "buffer" => {
            if let Some(buffer_egress) = state.stream_manager.get_buffer_egress() {
                // Arc<BufferEgress>
                // Update FPS
                if let Some(fps) = params.fps {
                    buffer_egress.set_fps(fps);
                    info!("BufferEgress FPS updated to {}", fps);
                }
                // Update encoding format
                if let Some(encoding_format) = params.encoding_format {
                    buffer_egress.set_encoding_format(encoding_format);
                    info!(
                        "BufferEgress encoding format updated to {:?}",
                        encoding_format
                    );
                }
                // Update max number of primitives
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    buffer_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "BufferEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }

                Json(UpdateEgressSettingsResponse {
                    message: "BufferEgress settings updated".to_string(),
                })
            } else {
                warn!("FileEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "FileEgress not initialized".to_string(),
                })
            }
        }
        "moq" => {
            if let Some(moq_egress) = state.stream_manager.get_moq_egress() {
                if let Some(fps) = params.fps {
                    moq_egress.set_fps(fps);
                    info!("MoqEgress FPS updated to {}", fps);
                }
                if let Some(encoding_format) = params.encoding_format {
                    moq_egress.set_encoding_format(encoding_format);
                    info!("MoqEgress encoding format updated to {:?}", encoding_format);
                }
                if let Some(max_primitives) = params.max_number_of_primitives() {
                    moq_egress.set_max_number_of_primitives(max_primitives);
                    info!(
                        "MoqEgress max_number_of_primitives updated to {}",
                        max_primitives
                    );
                }

                Json(UpdateEgressSettingsResponse {
                    message: "MoqEgress settings updated".to_string(),
                })
            } else {
                warn!("MoqEgress not initialized");
                Json(UpdateEgressSettingsResponse {
                    message: "MoqEgress not initialized".to_string(),
                })
            }
        }
        _ => {
            warn!("Unknown egress protocol: {}", params.egress_protocol);
            Json(UpdateEgressSettingsResponse {
                message: format!("Unknown egress protocol: {}", params.egress_protocol),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateEgressSettingsRequest;

    fn request_with_limits(
        max_number_of_primitives: Option<u64>,
        max_number_of_points: Option<u64>,
    ) -> UpdateEgressSettingsRequest {
        UpdateEgressSettingsRequest {
            fps: None,
            encoding_format: None,
            max_number_of_primitives,
            max_number_of_points,
            emit_with_ack: None,
            content_encoding: None,
            fec: None,
            fec_percentage: None,
            bandwidth: None,
            md5: None,
            egress_protocol: "Buffer".to_owned(),
        }
    }

    #[test]
    fn legacy_max_number_of_points_is_used_as_primitive_limit() {
        let request = request_with_limits(None, Some(100_000));

        assert_eq!(request.max_number_of_primitives(), Some(100_000));
    }

    #[test]
    fn max_number_of_primitives_takes_precedence_over_legacy_limit() {
        let request = request_with_limits(Some(50_000), Some(100_000));

        assert_eq!(request.max_number_of_primitives(), Some(50_000));
    }
}

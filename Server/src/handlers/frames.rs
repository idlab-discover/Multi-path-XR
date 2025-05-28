// handlers/frames.rs

use axum::extract::State;
use tracing::instrument;
use crate::types::AppState;
use axum::Json;

#[axum::debug_handler]
#[instrument(skip_all)]
pub async fn receive_frame(
    State(state): State<AppState>,
    frame_data: String,
) -> Json<serde_json::Value> {
    // Convert String to Vec<u8>
    let data = frame_data.as_bytes().to_vec();

    state.processing_pipeline.push_to_decoder(
        data,
        state.stream_manager.clone(),
        "manual".to_string()
    );

    Json(serde_json::json!({"status": "Frame pushed to processor"}))
}

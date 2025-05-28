// handlers/dash.rs

use std::{fs, path::PathBuf, time::Duration};

use axum::{extract::{Path, State}, response::{IntoResponse, Response}, http::StatusCode};
use crate::types::AppState;
use tracing::{debug, error, instrument};

#[instrument(skip_all)]
pub async fn fetch_dash_segment(
    State(app_state): State<AppState>,
    Path((stream_id, segment_name)): Path<(String, String)>,
) -> Response {
    let stream_manager = &app_state.stream_manager;

    let egress_option = stream_manager.get_buffer_egress();

    let egress = match egress_option {
        Some(e) => e,
        None => {
            error!("Buffer egress not initialized");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    if segment_name == "init.mp4" {
        if let Some(config) = egress.get_stream_config(&stream_id) {
            let init_segment = mp4_box::writer::create_init_segment(&config);
            return Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "video/mp4")
                .body(axum::body::Body::from(init_segment))
                .unwrap();
        } else {
            error!("Stream config for {} not found", stream_id);
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    if let Some(index_str) = segment_name.strip_suffix(".m4s").or_else(|| segment_name.strip_suffix(".mp4")) {
        let start_time = std::time::Instant::now();

        if let Ok(index) = index_str.parse::<u64>() {
            if let Some(frame) = egress.get_frame(&stream_id, index, Duration::from_millis(500)).await {
            let elapsed_time = start_time.elapsed();
            if elapsed_time > Duration::from_millis(30) {
                error!("Fetching frame took too long: {:?}", elapsed_time);
            }
            debug!("Serving frame with index {}", index);
            return Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "video/iso.segment")
                .body(axum::body::Body::from(frame.data.clone()))
                .unwrap();
            } else {
            error!("Frame index {} not found in buffer", index);
            return StatusCode::NOT_FOUND.into_response();
            }
        }
    }

    error!("Invalid segment requested: {}", segment_name);
    StatusCode::BAD_REQUEST.into_response()
}

#[instrument(skip_all)]
pub async fn fetch_dash_mpd(
    State(app_state): State<AppState>,
    Path(group_id): Path<String>,
) -> Response {
    let egress_option = app_state.stream_manager.get_buffer_egress();

    let egress = match egress_option {
        Some(e) => e,
        None => {
            error!("Buffer egress not initialized");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    
    // Remove .mpd from group_id if present
    let group_id = group_id.strip_suffix(".mpd").unwrap_or(&group_id).to_string();

    match egress.get_mpd(&group_id) {
        Some(xml) => {
            // Write the XML to a file for debugging
            let mut path = PathBuf::from("dist/exports");
            // Create the directory if it doesn't exist
            if let Err(e) = fs::create_dir_all(&path) {
                error!("Failed to create directory {:?}: {}", path, e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            path.push(format!("{}.mpd", group_id));
            if let Err(e) = fs::write(&path, &xml) {
                error!("Failed to write MPD to file {:?}: {}", path, e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/dash+xml")
            .body(axum::body::Body::from(xml))
            .unwrap()
        },
        None => {
            error!("MPD for group {} not found", group_id);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}
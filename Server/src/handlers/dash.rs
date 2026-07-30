// handlers/dash.rs

use std::{fs, path::PathBuf, time::Duration};

use crate::types::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tracing::{debug, error, instrument, warn};

fn origin_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn origin_now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

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
            let server_now_ms = origin_now_ms();

            return Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "video/mp4")
                .header("x-backend-now-ms", format!("{server_now_ms}"))
                .body(axum::body::Body::from(init_segment))
                .unwrap();
        } else {
            error!("Stream config for {} not found", stream_id);
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    if let Some(index_str) = segment_name
        .strip_suffix(".m4s")
        .or_else(|| segment_name.strip_suffix(".mp4"))
    {
        let start_time = std::time::Instant::now();

        if let Ok(index) = index_str.parse::<u64>() {
            match egress
                .get_frame(&stream_id, index, Duration::from_millis(500))
                .await
            {
                Ok(frame) => {
                    let elapsed_time = start_time.elapsed();
                    if elapsed_time > Duration::from_millis(120) {
                        warn!("Fetching frame {index} took too long: {elapsed_time:?}");
                    }
                    debug!("Serving frame with index {}", index);

                    let server_now_ms = origin_now_ms();

                    return Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "video/iso.segment")
                        .header("x-backend-wait-ms", format!("{}", elapsed_time.as_millis()))
                        .header("x-backend-now-ms", format!("{server_now_ms}"))
                        .body(axum::body::Body::from(frame.data.clone()))
                        .unwrap();
                }
                Err(err) => {
                    if let Some((first_index, last_index)) =
                        egress.get_first_and_last_frame_indices(&stream_id).await
                    {
                        error!(
                            "Frame with index {} not found in buffer (available range: {}-{})",
                            index, first_index, last_index
                        );
                    }

                    error!("Failed to retrieve frame with index {}: {}", index, err);
                    return StatusCode::NOT_FOUND.into_response();
                }
            }
        }
    }

    error!("Invalid segment requested: {}", segment_name);
    StatusCode::BAD_REQUEST.into_response()
}

#[instrument(skip_all)]
pub async fn fetch_clock_sync(State(app_state): State<AppState>) -> Response {
    let remote_receive_us = origin_now_us();
    let remote_send_us = origin_now_us();

    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(
            "Cache-Control",
            "no-store, no-cache, must-revalidate, max-age=0",
        )
        .header("Pragma", "no-cache")
        .header("Expires", "0")
        .header("x-clock-source-id", app_state.server_instance_id.as_str())
        .header("x-origin-now-ms", format!("{}", remote_send_us / 1_000))
        .header("x-origin-receive-us", format!("{remote_receive_us}"))
        .header("x-origin-send-us", format!("{remote_send_us}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

#[instrument(skip_all)]
pub async fn fetch_dash_origin_time(State(app_state): State<AppState>) -> Response {
    fetch_clock_sync(State(app_state)).await
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
    let group_id = group_id
        .strip_suffix(".mpd")
        .unwrap_or(&group_id)
        .to_string();

    match egress.get_mpd(&group_id) {
        Some(xml) => {
            // Write the XML to a file for debugging
            let mut path = PathBuf::from("dist/exports");
            // Create the directory if it doesn't exist
            if let Err(e) = fs::create_dir_all(&path) {
                error!("Failed to create directory {:?}: {}", path, e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            path.push(format!("{group_id}.mpd"));
            if let Err(e) = fs::write(&path, &xml) {
                error!("Failed to write MPD to file {:?}: {}", path, e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }

            let server_now_ms = origin_now_ms();

            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/dash+xml")
                .header("x-backend-now-ms", format!("{server_now_ms}"))
                .body(axum::body::Body::from(xml))
                .unwrap()
        }
        None => {
            error!("MPD for group {} not found", group_id);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::types::{AppState, MoqRelayStatus};

#[derive(Serialize)]
pub struct MoqConfigResponse {
    pub url: String,
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_ca_pem: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MoqRelayAnnouncementPayload {
    pub relay_url: String,
    pub namespace: String,
    #[serde(default)]
    pub announce_url: Option<String>,
}

#[derive(Serialize)]
pub struct MoqRelayListResponse {
    pub relays: Vec<MoqRelayStatus>,
}

pub async fn get_config(
    State(state): State<AppState>,
) -> Result<Json<MoqConfigResponse>, StatusCode> {
    let cfg = state.moq_config.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(MoqConfigResponse {
        url: cfg.url.clone(),
        namespace: cfg.namespace.clone(),
        tls_ca_pem: cfg.tls_ca_pem.clone(),
    }))
}

pub async fn announce(
    State(state): State<AppState>,
    Json(payload): Json<MoqRelayAnnouncementPayload>,
) -> Result<StatusCode, StatusCode> {
    info!("Received MoQ relay announcement: {:?}", payload);
    if payload.relay_url.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let status = MoqRelayStatus {
        relay_url: payload.relay_url,
        namespace: payload.namespace,
        announce_url: payload.announce_url,
        last_update_ms: Utc::now().timestamp_millis(),
    };
    info!(
        "MoQ relay announcement received: relay_url={}, namespace={}, announce_url={}",
        status.relay_url,
        status.namespace,
        status.announce_url.as_deref().unwrap_or("<unspecified>"),
    );

    state.moq_registry.update(status);
    Ok(StatusCode::ACCEPTED)
}

pub async fn list_relays(State(state): State<AppState>) -> Json<MoqRelayListResponse> {
    Json(MoqRelayListResponse {
        relays: state.moq_registry.snapshot(),
    })
}

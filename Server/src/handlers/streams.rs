use axum::extract::{Json, Query, State};
use tracing::instrument;
use crate::types::{AppState, EgressProtocolType};
use serde::{de, Deserialize, Deserializer, Serialize};

fn deserialize_csv_u8<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<&str> = Option::deserialize(deserializer)?;
    match s {
        Some(s) => {
            let vec = s
                .split(',')
                .map(|item| item.trim().parse::<u8>().map_err(de::Error::custom))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(vec))
        }
        None => Ok(None),
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct UpdateStreamSettingsRequest {
    pub stream_id: String,
    pub priority: Option<u8>,
    pub egress_protocols: Option<Vec<EgressProtocolType>>,
    pub process_incoming_frames: Option<bool>,
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub scale: Option<[f32; 3]>,
    pub presentation_time_offset: Option<u64>,
    pub decode_bypass: Option<bool>,
    pub aggregator_bypass: Option<bool>,
    pub ring_buffer_bypass: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_csv_u8")]
    pub max_point_percentages: Option<Vec<u8>>,   // e.g. [15, 25, 60]
}

#[derive(Serialize, Debug)]
pub struct UpdateStreamSettingsResponse {
    pub message: String,
}

#[instrument(skip_all)]
pub async fn update_stream_settings(
    Query(request): Query<UpdateStreamSettingsRequest>,
    State(state): State<AppState>,
) -> Json<UpdateStreamSettingsResponse> {
    let stream_manager = state.stream_manager.clone();

    // Get existing settings or create default
    let mut settings = stream_manager.get_stream_settings(&request.stream_id);

    // Update settings based on request
    if let Some(priority) = request.priority {
        settings.priority = priority;
    }

    if let Some(egress_protocols) = request.egress_protocols {
        settings.egress_protocols = egress_protocols;
    }


    if let Some(process_incoming_frames) = request.process_incoming_frames {
        settings.process_incoming_frames = process_incoming_frames;
    }

    if let Some(position) = request.position {
        settings.position = position;
    }

    if let Some(rotation) = request.rotation {
        settings.rotation = rotation;
    }
    
    if let Some(scale) = request.scale {
        settings.scale = scale;
    }

    if let Some(presentation_time_offset) = request.presentation_time_offset {
        settings.presentation_time_offset = Some(presentation_time_offset);
    }

    if let Some(decode_bypass) = request.decode_bypass {
        settings.decode_bypass = decode_bypass;        
    }

    if let Some(aggregator_bypass) = request.aggregator_bypass {
        settings.aggregator_bypass = aggregator_bypass;
    }

    if let Some(ring_buffer_bypass) = request.ring_buffer_bypass {
        settings.ring_buffer_bypass = ring_buffer_bypass;
    }

    if let Some(max_point_percentages) = request.max_point_percentages {
        settings.max_point_percentages = Some(max_point_percentages);
    }


    // Update the stream settings in StreamManager
    stream_manager.update_stream_settings(settings);

    Json(UpdateStreamSettingsResponse {
        message: format!("Stream settings updated for stream_id {}", request.stream_id),
    })
}

#[derive(Serialize, Debug)]
pub struct StreamListResponse {
    // We reuse the UpdateStreamSettingsRequest struct to represent the stream settings
    pub streams: Vec<UpdateStreamSettingsRequest>,
}

#[instrument(skip_all)]
pub async fn list_streams(State(state): State<AppState>) -> Json<StreamListResponse> {
    // Acquire a read-lock on the StreamManager’s stream_settings map
    let read_guard = state.stream_manager.stream_settings.read().unwrap();

    // Map all StreamSettings into a list of UpdateStreamSettingsRequest
    let all_settings = read_guard
        .values()
        .map(|settings| UpdateStreamSettingsRequest {
            stream_id: settings.stream_id.clone(),
            priority: Some(settings.priority),
            egress_protocols: Some(settings.egress_protocols.clone()),
            process_incoming_frames: Some(settings.process_incoming_frames),
            position: Some(settings.position),
            rotation: Some(settings.rotation),
            scale: Some(settings.scale),
            presentation_time_offset: settings.presentation_time_offset,

            decode_bypass: Some(settings.decode_bypass),
            aggregator_bypass: Some(settings.aggregator_bypass),
            ring_buffer_bypass: Some(settings.ring_buffer_bypass),
            max_point_percentages: settings.max_point_percentages.clone(),
        })
        .collect();

    Json(StreamListResponse { streams: all_settings })
}

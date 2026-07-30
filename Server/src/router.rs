use crate::handlers::{dash, datasets, frames, moq, scheduler, streams, websocket};
use crate::processing::ProcessingPipeline;
use crate::types::{ActiveJobs, AdvertisedMoqConfig, AppState, MoqRelayRegistry};
use crate::{handlers::egress, services};
use axum::{
    http::Request,
    routing::{get, post},
    Router,
};
use metrics::metrics_handler;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::{
    cors::CorsLayer,
    trace::{DefaultMakeSpan, TraceLayer},
};
use tracing::instrument;

#[instrument(skip_all)]
pub fn create_router(
    stream_manager: Arc<services::stream_manager::StreamManager>,
    processing_pipeline: Arc<ProcessingPipeline>,
    active_jobs: Arc<ActiveJobs>,
    moq_config: Option<AdvertisedMoqConfig>,
    moq_registry: Arc<MoqRelayRegistry>,
    server_instance_id: Arc<String>,
) -> Router {
    // Initialize SocketIo
    let (socket_io_layer, socket_io) = websocket::create_websocket_router_layer(
        stream_manager.clone(),
        server_instance_id.clone(),
    );

    let app_state = AppState {
        stream_manager: stream_manager.clone(),
        processing_pipeline,
        active_jobs,
        socket_io: Arc::new(socket_io),
        moq_config,
        moq_registry,
        server_instance_id,
    };

    stream_manager.set_socket_io(app_state.clone().socket_io.clone());

    Router::new()
        // Dash endpoints
        .route("/clock/sync", get(dash::fetch_clock_sync))
        .route("/dash/origin-time", get(dash::fetch_dash_origin_time))
        .route(
            "/dash/:stream_id/:segment_name",
            get(dash::fetch_dash_segment),
        )
        .route("/dash/:group_id.mpd", get(dash::fetch_dash_mpd))
        // Datasets endpoints
        .route("/datasets", get(datasets::list_datasets))
        .route("/datasets/list", get(datasets::list_datasets))
        .route("/datasets/ply_files", get(datasets::list_ply_files))
        .route("/datasets/dra_files", get(datasets::list_dra_files))
        // Egress endpoints
        .route(
            "/egress/update_settings",
            get(egress::update_egress_settings),
        )
        // Scheduler endpoints
        .route("/start_job", get(scheduler::start_transmission_job))
        .route("/stop_job", get(scheduler::stop_transmission_job))
        .route("/stop_all_jobs", get(scheduler::stop_all_jobs))
        // Frame endpoints
        .route("/frames/receive", post(frames::receive_frame)) // Manually insert a frame for transmission
        // Stream settings endpoint
        .route(
            "/streams/update_settings",
            get(streams::update_stream_settings),
        )
        .route("/streams/list", get(streams::list_streams))
        // Socket management
        .route("/sockets", get(websocket::list_sockets))
        .route("/sockets/list", get(websocket::list_sockets))
        .route("/sockets/clean", get(websocket::clean_sockets))
        // Metrics endpoint
        .route("/metrics", get(metrics_handler))
        .route("/moq/config", get(moq::get_config))
        .route("/moq/announce", post(moq::announce))
        .route("/moq/relays", get(moq::list_relays))
        // Apply middleware
        .layer(
            // We allow cross-origin requests from any origin
            CorsLayer::permissive(),
        )
        .layer(
            // Add logging middleware
            ServiceBuilder::new().layer(
                TraceLayer::new_for_http()
                    .make_span_with(DefaultMakeSpan::new().include_headers(true))
                    .on_request(
                        |request: &Request<axum::body::Body>, _span: &tracing::Span| {
                            #[instrument(skip_all, name = "request")]
                            fn log_request(request: &Request<axum::body::Body>) {
                                // If the path is /metrics, don't log it
                                if request.uri().path() == "/metrics"
                                    || request.uri().path() == "/dash/origin-time"
                                {
                                    return;
                                }

                                if request.uri().path().ends_with(".m4s") {
                                    return;
                                }

                                tracing::info!(
                                    "Received request for endpoint: {}",
                                    request.uri().path()
                                );
                            }
                            log_request(request);
                        },
                    ),
            ),
        )
        // SocketIo layer
        .layer(socket_io_layer)
        // Share state
        .with_state(app_state)
}

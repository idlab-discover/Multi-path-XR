use axum::body::Body;
use axum::extract::Query;
use axum::http::{header, HeaderValue, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::{extract::Json, http::StatusCode};
use axum::{routing::get, routing::post, Router};
use rayon::ThreadPool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use socketioxide::{
    extract::{Data, SocketRef},
    socket::DisconnectReason,
    SendError, SocketError, SocketIo,
};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::sync::Mutex;
use tower::ServiceBuilder;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::{debug, error, info, warn};

use crate::handlers::experiment::ExperimentHandler;
use crate::metrics_logger::{AgentMetricsSnapshot, MetricsLogger, MetricsLoggerError};

pub type ActiveJobs = Arc<tokio::sync::RwLock<HashMap<String, oneshot::Sender<()>>>>;

#[derive(Debug, Deserialize)]
struct AgentMetricsSocketPayload {
    node_id: String,
    last_scan_completed_at_ms: Option<u64>,
    last_scan_duration_ms: Option<u64>,
    scan_rounds_completed: u64,
    targets: Vec<crate::metrics_logger::AgentTargetMetricsSnapshot>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessOutput {
    level: String,
    data: String,
    location: Option<String>,
}

#[derive(Serialize)]
pub struct SimpleSocket {
    pub id: String,
    pub connected: bool,
}

#[derive(Serialize)]
pub struct SimpleSocketsResponse {
    pub sockets: Vec<SimpleSocket>,
}

pub async fn list_sockets(io: Arc<SocketIo>) -> Json<SimpleSocketsResponse> {
    let sockets = io.sockets();
    let mut simple_sockets = Vec::<SimpleSocket>::new();
    for socket in sockets {
        simple_sockets.push(SimpleSocket {
            id: socket.id.to_string(),
            connected: socket.connected(),
        });
    }
    Json(SimpleSocketsResponse {
        sockets: simple_sockets,
    })
}

// Clean up all the closed sockets + the ones in the list
pub async fn clean_sockets(io: Arc<SocketIo>, sockets: Vec<String>) -> Json<SimpleSocketsResponse> {
    let all_sockets = io.sockets();
    let mut cleaned_sockets = Vec::<SimpleSocket>::new();
    for socket in all_sockets {
        if !socket.connected() || sockets.contains(&socket.id.to_string()) {
            socket.clone().disconnect().ok();
            cleaned_sockets.push(SimpleSocket {
                id: socket.id.to_string(),
                connected: socket.connected(),
            });
        }
    }

    Json(SimpleSocketsResponse {
        sockets: cleaned_sockets,
    })
}

pub async fn find_node_id(
    socket_id: &str,
    agent_registry: &Arc<Mutex<HashMap<String, String>>>,
) -> Option<String> {
    let agent_registry = agent_registry.lock().await;
    // The key is the node id and the value is the socket id
    agent_registry.iter().find_map(|(node_id, socket)| {
        if socket == socket_id {
            Some(node_id.clone())
        } else {
            None
        }
    })
}

async fn shutdown_registered_agents(
    io: &SocketIo,
    agent_registry: &Arc<Mutex<HashMap<String, String>>>,
) {
    let node_ids = {
        let registry = agent_registry.lock().await;
        registry.keys().cloned().collect::<Vec<_>>()
    };

    for node_id in node_ids {
        let room_name = format!("agent_{node_id}");
        if let Err(err) = io.to(room_name).emit("shutdown_agent", &json!({})).await {
            error!("Failed to emit shutdown_agent to {node_id}: {err:?}");
        }
    }
}

async fn list_experiments() -> Json<serde_json::Value> {
    let dir = std::path::Path::new("./dist/experiments");
    let mut experiments = Vec::new();

    fn collect_experiments(
        current_dir: &std::path::Path,
        root_dir: &std::path::Path,
        experiments: &mut Vec<String>,
    ) {
        let Ok(entries) = fs::read_dir(current_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_experiments(&path, root_dir, experiments);
                continue;
            }

            if path.extension().is_some_and(|ext| ext == "yaml") {
                if let Ok(relative_path) = path.strip_prefix(root_dir) {
                    if let Some(name) = relative_path.to_str() {
                        experiments.push(name.replace('\\', "/"));
                    }
                }
            }
        }
    }

    collect_experiments(dir, dir, &mut experiments);
    experiments.sort();

    Json(json!({ "experiments": experiments }))
}

async fn current_experiment(
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let handler = experiment_handler.lock().await;
    match handler.get_current_experiment() {
        Some(experiment) => (
            StatusCode::OK,
            Json(json!({
                "status": "success",
                "experiment": experiment,
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "status": "error",
                "error": "No current experiment is loaded",
            })),
        ),
    }
}

async fn current_metrics_logger(
    experiment_handler: &Arc<Mutex<ExperimentHandler>>,
) -> Result<MetricsLogger, MetricsLoggerError> {
    let handler = experiment_handler.lock().await;
    handler.metrics_logger()
}

fn should_disable_browser_cache(path: &str) -> bool {
    path == "/"
        || path == "/list_experiments"
        || path.ends_with(".html")
        || path.ends_with(".js")
        || path.ends_with(".yaml")
        || path.ends_with(".yml")
}

async fn disable_browser_cache_for_static_assets(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_ascii_lowercase();
    let mut response = next.run(request).await;

    if response.status().is_success() && should_disable_browser_cache(&path) {
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
        );
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        headers.insert(header::EXPIRES, HeaderValue::from_static("0"));
    }

    response
}

#[derive(serde::Deserialize)]
struct ExecCommandQuery {
    node_id: String,
    command: String,
}

#[derive(serde::Serialize)]
struct ExecCommandResponse {
    status: String,
    message: Option<String>,
    error: Option<String>,
}

async fn exec_command_on_agent(
    Query(params): Query<ExecCommandQuery>,
    io: Arc<SocketIo>,
) -> (StatusCode, Json<ExecCommandResponse>) {
    let node_id = params.node_id;
    let command = params.command;

    info!(
        "Executing command '{}' on node '{}' inside agent",
        command, node_id
    );

    // Check if the room exists
    let room_name = format!("agent_{node_id}");
    let rooms = match io.rooms().await {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to get rooms: {:?}", err);
            vec![]
        }
    };
    // Print the room names
    let room_names = rooms.iter().map(|r| r.to_string()).collect::<Vec<String>>();
    if !room_names.contains(&room_name) {
        return (
            StatusCode::NOT_FOUND,
            Json(ExecCommandResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!("Node '{node_id}' is not connected")),
            }),
        );
    }

    // Send the command to the agent
    match io
        .to(format!("agent_{node_id}"))
        .emit("start_process", &command)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(ExecCommandResponse {
                status: "success".to_string(),
                message: Some(format!("Command sent to node '{node_id}'")),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ExecCommandResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!(
                    "Failed to send command to node '{node_id}': {err:?}"
                )),
            }),
        ),
    }
}

#[derive(serde::Deserialize)]
pub struct NetworkConditionData {
    pub(crate) node_id: String,
    pub(crate) bandwidth: String, // e.g. "200mbit"
    pub(crate) latency: String,   // e.g. "100ms"
    pub(crate) loss: String,      // e.g. "1.0%"
    pub(crate) htb_explicit_limits: Option<bool>,
    pub(crate) interface: Option<String>, // Optional interface name
    pub(crate) interface_ip: Option<String>,
}

#[derive(serde::Serialize)]
pub struct UpdateNetworkConditionsResponse {
    status: String,
    message: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct RouteWeightNexthop {
    pub via: String,
    pub dev: String,
    pub weight: u32,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct RouteWeightUpdateData {
    pub node_id: String,
    pub route: String,
    pub nexthops: Vec<RouteWeightNexthop>,
}

#[derive(serde::Serialize)]
pub struct UpdateRouteWeightsResponse {
    status: String,
    message: Option<String>,
    error: Option<String>,
}

pub async fn update_network_conditions_on_agent(
    Json(payload): Json<NetworkConditionData>,
    io: Arc<SocketIo>,
) -> (StatusCode, Json<UpdateNetworkConditionsResponse>) {
    let node_id = payload.node_id.clone();
    let bandwidth = payload.bandwidth;
    let latency = payload.latency;
    let loss = payload.loss;
    let htb_explicit_limits = payload.htb_explicit_limits.unwrap_or(false);
    let interface = payload.interface;
    let interface_ip = payload.interface_ip;

    // Construct the name of the room
    let room_name = format!("agent_{node_id}");

    // Check if the node (room) is connected
    let rooms = match io.rooms().await {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to get rooms: {:?}", err);
            vec![]
        }
    };
    let room_names = rooms.iter().map(|r| r.to_string()).collect::<Vec<String>>();
    if !room_names.contains(&room_name) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(UpdateNetworkConditionsResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!("Node '{node_id}' is not connected")),
            }),
        );
    }

    // Build a JSON payload to emit to the agent
    let emit_payload = json!({
        "bandwidth": bandwidth,
        "latency": latency,
        "loss": loss,
        "htb_explicit_limits": htb_explicit_limits,
        "interface": interface.unwrap_or("".to_string()), // Use empty string if interface is None
        "interface_ip": interface_ip.unwrap_or("".to_string()),
    });

    // Try sending the event to the agent
    match io.to(room_name).emit("update_network_conditions", &emit_payload).await {
        Ok(_) => {
            (
                StatusCode::OK,
                Json(UpdateNetworkConditionsResponse {
                    status: "success".to_string(),
                    message: Some(format!(
                        "Network conditions command sent to node '{node_id}': bw={bandwidth}, latency={latency}, loss={loss}"
                    )),
                    error: None
                })
            )
        }
        Err(err) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(UpdateNetworkConditionsResponse {
                    status: "error".to_string(),
                    message: None,
                    error: Some(format!(
                        "Failed to emit 'update_network_conditions' event to node '{node_id}': {err:?}"
                    ))
                })
            )
        }
    }
}

pub async fn update_route_weights_on_agent(
    Json(payload): Json<RouteWeightUpdateData>,
    io: Arc<SocketIo>,
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (StatusCode, Json<UpdateRouteWeightsResponse>) {
    let node_id = payload.node_id.clone();
    let room_name = format!("agent_{node_id}");

    let route_updates_allowed = {
        let handler = experiment_handler.lock().await;
        handler.route_updates_enabled()
    };

    if !route_updates_allowed {
        return (
            StatusCode::BAD_REQUEST,
            Json(UpdateRouteWeightsResponse {
                status: "error".to_string(),
                message: None,
                error: Some(
                    "Route updates are disabled for this experiment; enable GEANT weighted mode first"
                        .to_string(),
                ),
            }),
        );
    }

    let rooms = match io.rooms().await {
        Ok(r) => r,
        Err(err) => {
            error!("Failed to get rooms: {:?}", err);
            vec![]
        }
    };
    let room_names = rooms.iter().map(|r| r.to_string()).collect::<Vec<String>>();
    if !room_names.contains(&room_name) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(UpdateRouteWeightsResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!("Node '{node_id}' is not connected")),
            }),
        );
    }

    let emit_payload = json!({
        "route": payload.route,
        "nexthops": payload.nexthops,
    });

    match io
        .to(room_name)
        .emit("update_route_weights", &emit_payload)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(UpdateRouteWeightsResponse {
                status: "success".to_string(),
                message: Some(format!("Route weight update sent to node '{node_id}'")),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(UpdateRouteWeightsResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!(
                    "Failed to emit 'update_route_weights' event to node '{node_id}': {err:?}"
                )),
            }),
        ),
    }
}

fn generate_color_code(node_id: &str) -> u8 {
    // Use SHA-256 to hash the node_id for better distribution
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    let hash = hasher.finalize();
    let numeric_hash = u64::from_be_bytes(hash[0..8].try_into().unwrap());
    // Map the hash to a range of bright colors (e.g., 70-195 in ANSI 256-color palette)
    let bright_start = 70;
    let bright_end = 195;
    (numeric_hash % (bright_end - bright_start + 1) + bright_start) as u8
}

// Function to wrap text with ANSI color codes
fn colorize_text(text: &str, color_code: u8) -> String {
    format!("\x1b[38;5;{color_code}m[{text}]\x1b[0m")
}

#[derive(Deserialize)]
struct MetricsLatestQuery {
    instance: String,
    metric: String,
    n: Option<usize>, // defaults to 60 if not provided
    window_ms: Option<i64>,
}

#[derive(Serialize)]
struct MetricsLatestResponse {
    instance: String,
    metric: String,
    count: usize,
    values: Vec<(i64, f64)>,
    error: Option<String>,
}

// Get the latest metrics for a specific instance and metric
async fn get_latest_metrics(
    payload: MetricsLatestQuery,
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (StatusCode, Json<MetricsLatestResponse>) {
    let instance = payload.instance;
    let metric = payload.metric;
    let n = payload.n.unwrap_or(60);
    let window_ms = payload.window_ms;

    let values = match current_metrics_logger(&experiment_handler).await {
        Ok(logger) => {
            if let Some(window_ms) = window_ms {
                logger.get_window_ms(&instance, &metric, window_ms).await
            } else {
                logger.get_last_n(&instance, &metric, n).await
            }
        }
        Err(err) => Err(err),
    };
    match values {
        Ok(values) => (
            StatusCode::OK,
            Json(MetricsLatestResponse {
                instance,
                metric,
                count: values.len(),
                values,
                error: None,
            }),
        ),
        Err(
            MetricsLoggerError::LoggerNotInitialized
            | MetricsLoggerError::MissingData
            | MetricsLoggerError::NotRunning,
        ) => (
            StatusCode::NOT_FOUND,
            Json(MetricsLatestResponse {
                instance,
                metric,
                count: 0,
                values: Vec::new(),
                error: values.err().map(|err| format!("{err:?}")),
            }),
        ),
        Err(err) => {
            error!("Error fetching latest metrics: {err:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MetricsLatestResponse {
                    instance,
                    metric,
                    count: 0,
                    values: Vec::new(),
                    error: Some(format!("{err:?}")),
                }),
            )
        }
    }
}

#[derive(Serialize)]
struct PrometheusLikeResponse<T> {
    status: &'static str,
    data: T,
    error: Option<String>,
}

#[derive(Deserialize)]
struct MetricsInstanceQuery {
    instance: String,
}

#[derive(Serialize)]
struct MetricsInstanceDetails {
    instance: String,
    metrics: Vec<String>,
}

#[derive(Deserialize)]
struct PrometheusLikeQuery {
    query: String,
    instance: Option<String>,
    n: Option<usize>,
}

#[derive(Serialize)]
struct PrometheusLikeResultSeries {
    metric: serde_json::Map<String, Value>,
    values: Vec<(i64, String)>,
}

async fn list_metric_names_endpoint(
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (StatusCode, Json<PrometheusLikeResponse<Vec<String>>>) {
    match current_metrics_logger(&experiment_handler).await {
        Ok(logger) => {
            let metrics = logger.list_metric_names().await;
            (
                StatusCode::OK,
                Json(PrometheusLikeResponse {
                    status: "success",
                    data: metrics,
                    error: None,
                }),
            )
        }
        Err(MetricsLoggerError::LoggerNotInitialized) => (
            StatusCode::NOT_FOUND,
            Json(PrometheusLikeResponse {
                status: "error",
                data: Vec::new(),
                error: Some("metrics logger not initialized".to_string()),
            }),
        ),
        Err(err) => {
            error!("Failed to list metric names: {err:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PrometheusLikeResponse {
                    status: "error",
                    data: Vec::new(),
                    error: Some(format!("{err:?}")),
                }),
            )
        }
    }
}

async fn list_metric_instances_endpoint(
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (StatusCode, Json<PrometheusLikeResponse<Vec<String>>>) {
    match current_metrics_logger(&experiment_handler).await {
        Ok(logger) => {
            let instances = logger.list_instances();
            (
                StatusCode::OK,
                Json(PrometheusLikeResponse {
                    status: "success",
                    data: instances,
                    error: None,
                }),
            )
        }
        Err(MetricsLoggerError::LoggerNotInitialized) => (
            StatusCode::NOT_FOUND,
            Json(PrometheusLikeResponse {
                status: "error",
                data: Vec::new(),
                error: Some("metrics logger not initialized".to_string()),
            }),
        ),
        Err(err) => {
            error!("Failed to list metric instances: {err:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PrometheusLikeResponse {
                    status: "error",
                    data: Vec::new(),
                    error: Some(format!("{err:?}")),
                }),
            )
        }
    }
}

async fn list_metrics_for_instance_endpoint(
    Query(params): Query<MetricsInstanceQuery>,
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (
    StatusCode,
    Json<PrometheusLikeResponse<Vec<MetricsInstanceDetails>>>,
) {
    let logger = current_metrics_logger(&experiment_handler).await;
    let result = match logger {
        Ok(logger) => logger.list_metrics_for_instance(&params.instance).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(metrics) => (
            StatusCode::OK,
            Json(PrometheusLikeResponse {
                status: "success",
                data: vec![MetricsInstanceDetails {
                    instance: params.instance,
                    metrics,
                }],
                error: None,
            }),
        ),
        Err(MetricsLoggerError::LoggerNotInitialized | MetricsLoggerError::MissingData) => (
            StatusCode::NOT_FOUND,
            Json(PrometheusLikeResponse {
                status: "error",
                data: Vec::new(),
                error: Some("instance not found in metrics logger".to_string()),
            }),
        ),
        Err(err) => {
            error!(
                "Failed to list metrics for instance {}: {err:?}",
                params.instance
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PrometheusLikeResponse {
                    status: "error",
                    data: Vec::new(),
                    error: Some(format!("{err:?}")),
                }),
            )
        }
    }
}

async fn query_metric_endpoint(
    Query(params): Query<PrometheusLikeQuery>,
    experiment_handler: Arc<Mutex<ExperimentHandler>>,
) -> (
    StatusCode,
    Json<PrometheusLikeResponse<Vec<PrometheusLikeResultSeries>>>,
) {
    let metric = params.query.trim().to_string();
    let n = params.n.unwrap_or(60);
    if metric.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(PrometheusLikeResponse {
                status: "error",
                data: Vec::new(),
                error: Some("query must be a single metric name".to_string()),
            }),
        );
    }

    let logger = current_metrics_logger(&experiment_handler).await;
    let result = match logger {
        Ok(logger) => {
            logger
                .query_metric_series(&metric, params.instance.as_deref(), n)
                .await
        }
        Err(err) => Err(err),
    };
    match result {
        Ok(series) => {
            let data = series
                .into_iter()
                .map(|(instance, values)| {
                    let mut metric_obj = serde_json::Map::new();
                    metric_obj.insert("__name__".to_string(), Value::String(metric.clone()));
                    metric_obj.insert("instance".to_string(), Value::String(instance));
                    PrometheusLikeResultSeries {
                        metric: metric_obj,
                        values: values
                            .into_iter()
                            .map(|(ts, value)| (ts, value.to_string()))
                            .collect(),
                    }
                })
                .collect::<Vec<_>>();
            (
                StatusCode::OK,
                Json(PrometheusLikeResponse {
                    status: "success",
                    data,
                    error: None,
                }),
            )
        }
        Err(MetricsLoggerError::LoggerNotInitialized) => (
            StatusCode::NOT_FOUND,
            Json(PrometheusLikeResponse {
                status: "error",
                data: Vec::new(),
                error: Some("metrics logger not initialized".to_string()),
            }),
        ),
        Err(err) => {
            error!("Failed to query metric series: {err:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(PrometheusLikeResponse {
                    status: "error",
                    data: Vec::new(),
                    error: Some(format!("{err:?}")),
                }),
            )
        }
    }
}

pub fn create_router(_active_jobs: ActiveJobs, _thread_pool: Arc<ThreadPool>) -> Router {
    let experiment_handler = Arc::new(Mutex::new(ExperimentHandler::new()));
    let agent_registry = Arc::new(Mutex::new(HashMap::<String, String>::new()));

    let (layer, io) = SocketIo::new_layer();

    // Track connections and disconnections in the namespace
    let agent_registry_clone = Arc::clone(&agent_registry);
    let experiment_handler_for_ws = Arc::clone(&experiment_handler);
    io.ns("/", move |socket: SocketRef| {
        let socket_id = socket.id.to_string();
        debug!("Setting up websocket connection with id {:#?}", socket_id);

        let agent_registry_for_disconnect = Arc::clone(&agent_registry);
        socket.on_disconnect(move |socket: SocketRef, reason: DisconnectReason| {
            let agent_registry_for_disconnect = Arc::clone(&agent_registry_for_disconnect);
            async move {
            info!(
                "Socket {} on ns {} disconnected, reason: {:?}",
                socket.id,
                socket.ns(),
                reason
            );
                let socket_id = socket.id.to_string();
                let mut registry = agent_registry_for_disconnect.lock().await;
                registry.retain(|_, registered_socket_id| registered_socket_id != &socket_id);
            }
        });

        let agent_registry_clone = Arc::clone(&agent_registry);
        socket.on("process_output", {
            let agent_registry = agent_registry_clone.clone();
            move |s: SocketRef, Data(payload): Data<ProcessOutput>| async move {
                // The payload is a JSON object with the following structure: { "level": "info", "data": "some data" }
                let message_level = if payload.level.trim().is_empty() {
                    "info".to_string()
                } else {
                    payload.level.trim().to_ascii_lowercase()
                };
                let message_data = payload.data;
                // If the message data is empty (also after trimming), do not log it
                if message_data.trim().is_empty() {
                    return;
                }
                // Get the node id from the socket id
                let node_id = find_node_id(&s.id.to_string(), &agent_registry)
                    .await
                    .unwrap_or_else(|| "unknown".to_string());

                // Generate a color code for the node_id
                let color_code = generate_color_code(&node_id);
                let colored_node_id = colorize_text(&node_id, color_code);

                match message_level.as_str() {
                    "trace" | "debug" => {
                        if let Some(location) = payload.location {
                            debug!("{} {}; {}", colored_node_id, location, message_data);
                        } else {
                            debug!("{} {}", colored_node_id, message_data);
                        }
                    }
                    "info" => {
                        if let Some(location) = payload.location {
                            info!("{} {}; {}", colored_node_id, location, message_data);
                        } else {
                            info!("{} {}", colored_node_id, message_data);
                        }
                    }
                    "warn" | "warning" => {
                        if let Some(location) = payload.location {
                            warn!("{} {}; {}", colored_node_id, location, message_data);
                        } else {
                            warn!("{} {}", colored_node_id, message_data);
                        }
                    }
                    _ => {
                        if let Some(location) = payload.location {
                            error!("{} {}: {}", colored_node_id, location, message_data);
                        } else {
                            error!("{} {}", colored_node_id, message_data);
                        }
                    }
                }
            }
        });

        socket.on("agent_metrics_snapshot", {
            let experiment_handler = Arc::clone(&experiment_handler_for_ws);
            move |_s: SocketRef, Data(payload): Data<serde_json::Value>| async move {
                match serde_json::from_value::<AgentMetricsSocketPayload>(payload.clone()) {
                    Ok(payload_converted) => {
                        let snapshot = AgentMetricsSnapshot {
                            node_id: payload_converted.node_id,
                            last_scan_completed_at_ms: payload_converted.last_scan_completed_at_ms,
                            last_scan_duration_ms: payload_converted.last_scan_duration_ms,
                            scan_rounds_completed: payload_converted.scan_rounds_completed,
                            targets: payload_converted.targets,
                        };

                        let result = match current_metrics_logger(&experiment_handler).await {
                            Ok(logger) => logger.ingest_agent_snapshot(snapshot).await,
                            Err(err) => Err(err),
                        };
                        match result {
                            Ok(()) => {}
                            Err(
                                MetricsLoggerError::LoggerNotInitialized
                                | MetricsLoggerError::NotRunning,
                            ) => {
                                debug!(
                                    "Ignoring agent metrics snapshot because the metrics logger is not accepting snapshots"
                                );
                            }
                            Err(err) => {
                                error!("Failed to ingest agent metrics snapshot: {err:?}");
                            }
                        }
                    }
                    Err(e) => {
                        error!("Typed deserialization failed for agent_metrics_snapshot: {}", e);
                    }
                }
            }
        });

        socket.on("route_update_result", move |s: SocketRef, Data(payload): Data<Value>| async move {
            info!("Route update result from socket {}: {}", s.id, payload);
        });

        // This payload only contains the node id
        let agent_registry_clone = agent_registry.clone();
        socket.on(
            "agent_ready",
            move |s: SocketRef, Data(node_id): Data<String>| {
                let agent_registry = agent_registry_clone.clone();
                s.join(format!("agent_{node_id}"));
                async move {
                    let socket_id = s.id.to_string();
                    info!(
                        "WebSocket id: {:#?} belongs to the agent of {}",
                        socket_id, node_id
                    );
                    // Store the socket id
                    let mut agent_registry = agent_registry.lock().await;
                    agent_registry.insert(node_id.clone(), socket_id);
                }
            },
        );

        // There are two issues with the Rust socket.io libraries for the server and the client:
        // 1. The server library (socketioxide) -for some reason- occasionaly closes the first socket connection some short time after the client connects. It is not clear why this happens. Luckily, the client library (rust-socketio) is able to reconnect automatically. However, the server leaves the closed socket in the active list and sometimes does not detect the closed connection.
        // 2. The client library (rust-socketio) does not provide any ability to get the socket id of the client.
        // The code below is a workaround to get the socket id of the client by sending an event to the client 2 seconds after the client connects.
        // This way, the client can expose the socket id artificially through a message from the server.
        // Additionally, the first connection, which is closed within those first two seconds, will be detected and removed from the active list.
        tokio::spawn(async move {
            let socket_id = socket.id.to_string();
            // Wait for a few seconds before sending the event
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            // Handle the `Result` returned by `emit_with_ack`
            match socket.emit_with_ack::<String, Value>("has_connected", &socket_id) {
                Ok(ack_stream) => {
                    // Now handle the asynchronous `AckStream`
                    match ack_stream.await {
                        Ok(_) => info!("Websocket connected with id: {:#?}", socket_id),
                        Err(err) => {
                            error!("Ack error from socket {}: {:?}", socket_id, err);
                        }
                    }
                }
                Err(SendError::Socket(socket_error)) => {
                    match socket_error {
                        // Handle the case where the socket is closed
                        SocketError::Closed => {
                            error!(
                                "Socket {} is closed. Removing it from active list.",
                                socket_id
                            );
                            // Disconnect the socket and perform any additional cleanup if needed
                            if let Err(err) = socket.disconnect() {
                                error!("Failed to disconnect socket {}: {:?}", socket_id, err);
                            }
                        }
                        _ => {
                            // Handle other socket errors
                            error!(
                                "Failed to send 'has_connected' event for socket {}: {:?}",
                                socket_id, socket_error
                            );
                        }
                    }
                }
                Err(SendError::Serialize(err)) => {
                    // Handle serialization errors
                    error!(
                        "Failed to serialize 'has_connected' event for socket {}: {:?}",
                        socket_id, err
                    );
                }
            }
        });
    });

    let agent_registry1 = agent_registry_clone.clone();
    let agent_registry2 = agent_registry_clone.clone();
    let agent_registry3 = agent_registry_clone.clone();
    Router::new()
        .nest_service("/", ServeDir::new("dist"))
        .route(
            "/list_sockets",
            get({
                let io_clone = io.clone();
                move || list_sockets(io_clone.clone().into())
            }),
        )
        .route(
            "/list_agents",
            get({
                let agent_registry = agent_registry1.clone();
                move || {
                    let agent_registry = agent_registry.clone();
                    async move {
                        let agent_registry = agent_registry.lock().await;
                        Json(agent_registry.clone())
                    }
                }
            }),
        )
        .route(
            "/clean_sockets",
            get({
                let io_clone = io.clone();
                let additional_sockets = vec!["socket_id_1".to_string(), "socket_id_2".to_string()];
                move || clean_sockets(io_clone.clone().into(), additional_sockets.clone())
            }),
        )
        .route("/list_experiments", get(list_experiments))
        .route(
            "/current_experiment",
            get({
                let handler = experiment_handler.clone();
                move || current_experiment(handler.clone())
            }),
        )
        .route(
            "/start_environment",
            post({
                let handler = experiment_handler.clone();
                let io_clone = io.clone();
                let agent_registry = agent_registry2.clone();
                move |Json(payload): Json<HashMap<String, String>>| {
                    let handler = handler.clone();
                    let agent_registry = agent_registry.clone();
                    let io_clone = io_clone.clone();
                    async move {
                        // Clone the value or use default
                        let experiment = payload
                            .get("experimentName")
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());
                        let environment = payload
                            .get("environment")
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());

                        // Run startup in a detached task so client disconnects don't cancel backend startup.
                        let handler_for_task = handler.clone();
                        let agent_registry_for_task = agent_registry.clone();
                        let io_for_task = io_clone.clone();
                        let env_for_task = environment.clone();
                        let exp_for_task = experiment.clone();

                        let start_task = tokio::spawn(async move {
                            let mut handler = handler_for_task.lock().await;
                            handler
                                .start_environment(
                                    &env_for_task,
                                    &exp_for_task,
                                    io_for_task.into(),
                                    agent_registry_for_task.clone(),
                                )
                                .await
                        });

                        match start_task.await {
                            Ok(Ok(message)) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Ok(Err(error)) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                            Err(join_error) => Json(serde_json::json!({
                                "status": "error",
                                "error": format!("start task failed: {:?}", join_error)
                            })),
                        }
                    }
                }
            }),
        )
        .route(
            "/stop",
            get({
                let handler = experiment_handler.clone();
                let agent_registry_clone = agent_registry3.clone();
                let io_clone = io.clone();
                move || {
                    let agent_registry = agent_registry_clone.clone();
                    let handler = handler.clone();
                    let io_clone = io_clone.clone();
                    async move {
                        shutdown_registered_agents(&io_clone, &agent_registry).await;

                        let mut handler = handler.lock().await;
                        let result = match handler.stop_environment().await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        };

                        // Clean up the agent registry
                        let mut agent_registry = agent_registry.lock().await;
                        agent_registry.clear();

                        result
                    }
                }
            }),
        )
        .route(
            "/start_actions",
            post({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.start_actions().await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/cleanup_environment_processes",
            post({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.cleanup_environment_processes().await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/exec",
            get({
                let handler = experiment_handler.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.exec_command(params).await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/nodes",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.get_nodes().await {
                            Ok(nodes) => Json(nodes),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/links",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.get_links().await {
                            Ok(links) => Json(links),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/status",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.get_status().await {
                            Ok(status) => Json(status),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/visualize",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.get_visualization().await {
                            Ok(image_bytes) => (
                                axum::http::StatusCode::OK,
                                [("Content-Type", "image/png")],
                                image_bytes,
                            ),
                            Err(error) => (
                                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                [("Content-Type", "application/json")],
                                serde_json::to_vec(
                                    &serde_json::json!({ "status": "error", "error": error }),
                                )
                                .unwrap(),
                            ),
                        }
                    }
                }
            }),
        )
        .route(
            "/start_xterm",
            get({
                let handler = experiment_handler.clone();
                move |Query(params): Query<HashMap<String, String>>| {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.start_xterm(params).await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/ping_all",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.ping_all().await {
                            Ok(results) => Json(results),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/tunnels",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.list_tunnels().await {
                            Ok(tunnels) => Json(tunnels),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/tunnels/open",
            post({
                let handler = experiment_handler.clone();
                move |Json(params): Json<HashMap<String, String>>| {
                    let handler = handler.clone();
                    async move {
                        let handler = handler.lock().await;
                        match handler.open_tunnel(params).await {
                            Ok(tunnel) => Json(serde_json::json!({
                                "status": "success",
                                "tunnel": tunnel
                            })),
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/tunnels/close",
            post({
                let handler = experiment_handler.clone();
                move |Json(params): Json<HashMap<String, String>>| {
                    let handler = handler.clone();
                    async move {
                        let id = params
                            .get("id")
                            .cloned()
                            .or_else(|| params.get("tunnel_id").cloned());
                        if id.is_none() {
                            return Json(serde_json::json!({
                                "status": "error",
                                "error": "Missing `id`"
                            }));
                        }
                        let id = id.unwrap();
                        let handler = handler.lock().await;
                        match handler.close_tunnel(&id).await {
                            Ok(message) => {
                                Json(serde_json::json!({ "status": "success", "message": message }))
                            }
                            Err(error) => {
                                Json(serde_json::json!({ "status": "error", "error": error }))
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/exec_on_agent",
            get({
                let io_clone = io.clone();
                move |Query(payload): Query<ExecCommandQuery>| {
                    exec_command_on_agent(Query(payload), io_clone.clone().into())
                }
            }),
        )
        .route(
            "/update_network_conditions",
            post({
                let io_clone = io.clone();
                move |Json(payload): Json<NetworkConditionData>| {
                    update_network_conditions_on_agent(Json(payload), io_clone.clone().into())
                }
            }),
        )
        .route(
            "/update_route_weights",
            post({
                let io_clone = io.clone();
                let handler = experiment_handler.clone();
                move |Json(payload): Json<RouteWeightUpdateData>| {
                    update_route_weights_on_agent(
                        Json(payload),
                        io_clone.clone().into(),
                        handler.clone(),
                    )
                }
            }),
        )
        .route(
            "/get_latest_metrics",
            get({
                let handler = experiment_handler.clone();
                move |Query(params): Query<MetricsLatestQuery>| {
                    let handler = handler.clone();
                    get_latest_metrics(params, handler)
                }
            }),
        )
        .route(
            "/debug/prometheus/api/v1/label/__name__/values",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    list_metric_names_endpoint(handler)
                }
            }),
        )
        .route(
            "/debug/metrics/instances",
            get({
                let handler = experiment_handler.clone();
                move || {
                    let handler = handler.clone();
                    list_metric_instances_endpoint(handler)
                }
            }),
        )
        .route(
            "/debug/metrics/instance",
            get({
                let handler = experiment_handler.clone();
                move |Query(params): Query<MetricsInstanceQuery>| {
                    let handler = handler.clone();
                    list_metrics_for_instance_endpoint(Query(params), handler)
                }
            }),
        )
        .route(
            "/debug/prometheus/api/v1/query",
            get({
                let handler = experiment_handler.clone();
                move |Query(params): Query<PrometheusLikeQuery>| {
                    let handler = handler.clone();
                    query_metric_endpoint(Query(params), handler)
                }
            }),
        )
        .layer(CorsLayer::permissive()) // Enable CORS policy
        .layer(
            ServiceBuilder::new().layer(
                TraceLayer::new_for_http()
                    .make_span_with(DefaultMakeSpan::new().include_headers(true))
                    .on_request(|request: &Request<_>, _span: &tracing::Span| {
                        let uri_path = request.uri().path();

                        // Do not log if path is /get_latest_metrics
                        if uri_path == "/get_latest_metrics" {
                            return;
                        }

                        tracing::info!("Received request for endpoint: {}", uri_path);
                    }),
            ),
        )
        .layer(middleware::from_fn(disable_browser_cache_for_static_assets))
        .layer(layer)
}

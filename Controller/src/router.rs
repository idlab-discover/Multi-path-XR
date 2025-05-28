use axum::extract::Query;
use axum::http::Request;
use axum::{routing::get, routing::post, Router};
use axum::{extract::Json, http::StatusCode};
use std::fs;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;
use tracing::{info, error, debug};
use sha2::{Sha256, Digest};
use tower::ServiceBuilder;
use tower_http::{cors::CorsLayer, services::ServeDir};
use tower_http::trace::{TraceLayer, DefaultMakeSpan};
use socketioxide::{SocketIo, extract::{Data, SocketRef}, socket::DisconnectReason, SendError, SocketError};
use rayon::ThreadPool;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::handlers::experiment::ExperimentHandler;

pub type ActiveJobs = Arc<tokio::sync::RwLock<HashMap<String, oneshot::Sender<()>>>>;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessOutput {
    level: String,
    data: String,
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

pub async fn list_sockets(
    io: Arc<SocketIo>,
) -> Json<SimpleSocketsResponse> {
    let sockets = io.sockets().unwrap_or_default();
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
pub async fn clean_sockets(
    io: Arc<SocketIo>,
    sockets: Vec<String>,
) -> Json<SimpleSocketsResponse> {
    let all_sockets = io.sockets().unwrap_or_default();
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

pub async fn find_node_id(socket_id: &str, agent_registry: &Arc<Mutex<HashMap<String, String>>>) -> Option<String> {
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

async fn list_experiments() -> Json<serde_json::Value> {
    let dir = "./dist/experiments"; // Directory containing YAML files
    let mut experiments = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "yaml") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    experiments.push(name.to_string());
                }
            }
        }
    }

    Json(json!({ "experiments": experiments }))
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
    io: Arc<SocketIo>
) -> (StatusCode, Json<ExecCommandResponse>) {
    let node_id = params.node_id;
    let command = params.command;

    info!("Executing command '{}' on node '{}'", command, node_id);

    // Check if the room exists
    let room_name = format!("agent_{}", node_id);
    let rooms = io.rooms().unwrap_or_default();
    // Print the room names
    let room_names = rooms.iter().map(|r| r.to_string()).collect::<Vec<String>>();
    if !room_names.contains(&room_name) {
        return (
            StatusCode::NOT_FOUND,
            Json(ExecCommandResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!("Node '{}' is not connected", node_id)),
            }),
        );
    }


    // Send the command to the agent
    match io.to(format!("agent_{}", node_id)).emit("start_process", &command) {
        Ok(_) => (
            StatusCode::OK,
            Json(ExecCommandResponse {
                status: "success".to_string(),
                message: Some(format!("Command sent to node '{}'", node_id)),
                error: None,
            }),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ExecCommandResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!(
                    "Failed to send command to node '{}': {:?}",
                    node_id, err
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
    pub(crate) interface: Option<String>, // Optional interface name
}

#[derive(serde::Serialize)]
pub struct UpdateNetworkConditionsResponse {
    status: String,
    message: Option<String>,
    error: Option<String>,
}

pub async fn update_network_conditions_on_agent(
    Json(payload): Json<NetworkConditionData>,
    io: Arc<SocketIo>
) -> (StatusCode, Json<UpdateNetworkConditionsResponse>) {
    let node_id = payload.node_id.clone();
    let bandwidth = payload.bandwidth;
    let latency = payload.latency;
    let loss = payload.loss;
    let interface = payload.interface;

    // Construct the name of the room
    let room_name = format!("agent_{}", node_id);

    // Check if the node (room) is connected
    let rooms = io.rooms().unwrap_or_default();
    let room_names = rooms.iter().map(|r| r.to_string()).collect::<Vec<String>>();
    if !room_names.contains(&room_name) {
        return (
            StatusCode::NOT_FOUND,
            Json(UpdateNetworkConditionsResponse {
                status: "error".to_string(),
                message: None,
                error: Some(format!("Node '{}' is not connected", node_id)),
            }),
        );
    }

    // Build a JSON payload to emit to the agent
    let emit_payload = json!({
        "bandwidth": bandwidth,
        "latency": latency,
        "loss": loss,
        "interface": interface.unwrap_or("".to_string()), // Use empty string if interface is None
    });

    // Try sending the event to the agent
    match io.to(room_name).emit("update_network_conditions", &emit_payload) {
        Ok(_) => {
            (
                StatusCode::OK,
                Json(UpdateNetworkConditionsResponse {
                    status: "success".to_string(),
                    message: Some(format!(
                        "Network conditions command sent to node '{}': bw={}, latency={}, loss={}",
                        node_id, bandwidth, latency, loss
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
                        "Failed to emit 'update_network_conditions' event to node '{}': {:?}",
                        node_id, err
                    ))
                })
            )
        }
    }
}
   

fn generate_color_code(node_id: &str) -> u8 {
    // Use SHA-256 to hash the node_id for better distribution
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    let hash = hasher.finalize();
    let numeric_hash = u64::from_be_bytes(hash[0..8].try_into().unwrap());
    // Map the hash to a range of bright colors (e.g., 70–195 in ANSI 256-color palette)
    let bright_start = 70;
    let bright_end = 195;
    (numeric_hash % (bright_end - bright_start + 1) + bright_start) as u8
}

// Function to wrap text with ANSI color codes
fn colorize_text(text: &str, color_code: u8) -> String {
    format!("\x1b[38;5;{}m[{}]\x1b[0m", color_code, text)
}

pub fn create_router(_active_jobs: ActiveJobs, _thread_pool: Arc<ThreadPool>) -> Router {
    let experiment_handler = Arc::new(Mutex::new(ExperimentHandler::new()));
    let agent_registry = Arc::new(Mutex::new(HashMap::<String, String>::new()));

    let (layer, io) = SocketIo::new_layer();

    // Track connections and disconnections in the namespace
    let agent_registry_clone = Arc::clone(&agent_registry);
    io.ns("/",  move |socket: SocketRef| {
        let socket_id = socket.id.to_string();
        debug!("Setting up websocket connection with id {:#?}", socket_id);

        socket.on_disconnect(|socket: SocketRef, reason: DisconnectReason| async move {
            info!("Socket {} on ns {} disconnected, reason: {:?}", socket.id, socket.ns(), reason);
            socket.leave("broadcast").unwrap();
        });

        let agent_registry_clone = Arc::clone(&agent_registry);
        socket.on("process_output", {
            let agent_registry = agent_registry_clone.clone();
            move |s: SocketRef, Data(payload): Data<ProcessOutput>| async move {
                // The payload is a JSON object with the following structure: { "level": "info", "data": "some data" }
                let message_level = if payload.level.is_empty() {
                    "info".to_string()
                } else {
                    payload.level
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
        
                if message_level == "info" {
                    info!("{} {}", colored_node_id, message_data);
                } else {
                    error!("{} {}", colored_node_id, message_data);
                }
            }
        });

        // This payload only contains the node id
        let agent_registry_clone = agent_registry.clone();
        socket.on("agent_ready", move |s: SocketRef, Data(node_id): Data<String>| {
            let agent_registry = agent_registry_clone.clone();
            s.join(format!("agent_{}", node_id)).unwrap();
            async move {
                let socket_id = s.id.to_string();
                info!("WebSocket id: {:#?} belongs to the agent of {}", socket_id, node_id);
                // Store the socket id
                let mut agent_registry = agent_registry.lock().await;
                agent_registry.insert(node_id.clone(), socket_id);
            }
        });

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
                        },
                    }
                }
                Err(SendError::Socket(socket_error)) => {
                    match socket_error {
                        // Handle the case where the socket is closed
                        SocketError::Closed => {
                            error!("Socket {} is closed. Removing it from active list.", socket_id);
                            // Disconnect the socket and perform any additional cleanup if needed
                            if let Err(err) = socket.disconnect() {
                                error!("Failed to disconnect socket {}: {:?}", socket_id, err);
                            }
                        }
                        _ => {
                            // Handle other socket errors
                            error!("Failed to send 'has_connected' event for socket {}: {:?}", socket_id, socket_error);
                        }
                    }
                }
                Err(SendError::Serialize(err)) => {
                    // Handle serialization errors
                    error!("Failed to serialize 'has_connected' event for socket {}: {:?}", socket_id, err);
                }
            }
        });
    });

    
    let agent_registry1 = agent_registry_clone.clone();
    let agent_registry2 = agent_registry_clone.clone();
    Router::new()
        .nest_service("/", ServeDir::new("dist"))
        .route("/list_sockets", get({
            let io_clone = io.clone();
            move || list_sockets(io_clone.clone().into())
        }))
        .route("/list_agents", get({
            let agent_registry = agent_registry1.clone();
            move || {
                let agent_registry = agent_registry.clone();
                async move {
                    let agent_registry = agent_registry.lock().await;
                    Json(agent_registry.clone())
                }
            }
        }))
        .route("/clean_sockets", get({
            let io_clone = io.clone();
            let additional_sockets = vec!["socket_id_1".to_string(), "socket_id_2".to_string()];
            move || clean_sockets(io_clone.clone().into(), additional_sockets.clone())
        }))
        .route("/list_experiments", get({
            list_experiments
        }))
        .route("/start_environment", post({
            let handler = experiment_handler.clone();
            let io_clone = io.clone();
            move |Json(payload): Json<HashMap<String, String>>| {
                let handler = handler.clone();
                async move {
                    let mut handler = handler.lock().await;
                    // Clone the value or use default
                    let experiment = payload.get("experimentName").cloned().unwrap_or_else(|| "unknown".to_string());
                    let environment = payload.get("environment").cloned().unwrap_or_else(|| "unknown".to_string());


                    match handler.start_environment(&environment, &experiment, io_clone.into()).await {
                        Ok(message) => Json(serde_json::json!({ "status": "success", "message": message })),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/stop", get({
            let handler = experiment_handler.clone();
            let agent_registry_clone = agent_registry2.clone();
            move || {
                let agent_registry = agent_registry_clone.clone();
                let handler = handler.clone();
                async move {
                    let mut handler = handler.lock().await;
                    let result = match handler.stop_environment().await {
                        Ok(message) => Json(serde_json::json!({ "status": "success", "message": message })),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    };

                    // Clean up the agent registry
                    let mut agent_registry = agent_registry.lock().await;
                    agent_registry.clear();

                    result
                }
            }
        }))
        .route("/exec", get({
            let handler = experiment_handler.clone();
            move |Query(params): Query<HashMap<String, String>>| {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.exec_command(params).await {
                        Ok(message) => Json(serde_json::json!({ "status": "success", "message": message })),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/nodes", get({
            let handler = experiment_handler.clone();
            move || {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.get_nodes().await {
                        Ok(nodes) => Json(nodes),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/links", get({
            let handler = experiment_handler.clone();
            move || {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.get_links().await {
                        Ok(links) => Json(links),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/status", get({
            let handler = experiment_handler.clone();
            move || {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.get_status().await {
                        Ok(status) => Json(status),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/visualize", get({
            let handler = experiment_handler.clone();
            move || {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.get_visualization().await {
                        Ok(image_bytes) => {
                            (
                                axum::http::StatusCode::OK,
                                [("Content-Type", "image/png")],
                                image_bytes,
                            )
                        },
                        Err(error) => {
                            (
                                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                [("Content-Type", "application/json")],
                                serde_json::to_vec(&serde_json::json!({ "status": "error", "error": error })).unwrap(),
                            )
                        },
                    }
                }
            }
        }))
        .route("/start_xterm", get({
            let handler = experiment_handler.clone();
            move |Query(params): Query<HashMap<String, String>>| {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.start_xterm(params).await {
                        Ok(message) => Json(serde_json::json!({ "status": "success", "message": message })),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route("/ping_all", get({
            let handler = experiment_handler.clone();
            move || {
                let handler = handler.clone();
                async move {
                    let handler = handler.lock().await;
                    match handler.ping_all().await {
                        Ok(results) => Json(results),
                        Err(error) => Json(serde_json::json!({ "status": "error", "error": error })),
                    }
                }
            }
        }))
        .route(
            "/exec_on_agent",
            get({
                let io_clone = io.clone();
                move |Query(payload): Query<ExecCommandQuery>| {
                    exec_command_on_agent(Query(payload), io_clone.clone().into())
                }
            })
        )
        .route(
            "/update_network_conditions",
            post({
                let io_clone = io.clone();
                move |Json(payload): Json<NetworkConditionData>| {
                    update_network_conditions_on_agent(Json(payload), io_clone.clone().into())
                }
            })
        )
        .layer(CorsLayer::permissive()) // Enable CORS policy
        .layer(
            ServiceBuilder::new()
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(
                            DefaultMakeSpan::new().include_headers(true)
                        )
                        .on_request(
                        |request: &Request<_>, _span: &tracing::Span| {
                            tracing::info!(
                                "Received request for endpoint: {}",
                                request.uri().path()
                            );
                        })
                )
        )
        .layer(layer)
}

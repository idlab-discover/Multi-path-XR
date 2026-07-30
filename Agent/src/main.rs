mod cli;
mod curl;
mod logging;
mod metrics;
mod networking;
mod process_manager;
mod runtime;

use clap::Parser;
#[cfg(target_os = "linux")]
use libc::{self, PR_SET_CHILD_SUBREAPER};
use rust_socketio::{ClientBuilder, Payload, RawClient};
use serde_json::json;
use signal_hook::consts::TERM_SIGNALS;
#[cfg(target_os = "linux")]
use signal_hook::iterator::Signals;
use std::collections::BTreeSet;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::layer::{Layer, SubscriberExt};

use crate::cli::Args;
use crate::curl::handle_curl_event;
use crate::logging::{emit_log, set_application_log_client, ApplicationLoggingLayer};
use crate::metrics::{
    start_metrics_forwarder, start_metrics_scanner, MetricsForwarderConfig, MetricsScannerConfig,
    MetricsStore,
};
use crate::networking::{
    apply_route_update, extract_port_from_url, reset_tc_state_on_startup,
    resolve_network_condition_interfaces, set_network_conditions, RouteUpdateRequest,
};
use crate::process_manager::{
    kill_duplicate_processes, reap_exited_processes, shutdown_managed_processes, start_process,
    stop_process,
};
use crate::runtime::{clean_threads, join_all_threads};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(PR_SET_CHILD_SUBREAPER, 1);
    }

    let args = Args::parse();
    initialize_logging(args.log_level.as_level_filter())?;

    info!("Starting agent");
    info!("{:?}", args);

    if let Err(e) = kill_duplicate_processes(&args.node_id) {
        error!("Failed to check for duplicate processes: {}", e);
        return Err(e);
    }

    if let Err(err) = reset_tc_state_on_startup() {
        warn!("Failed to reset tc state on startup: {}", err);
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let websocket_connected = Arc::new(AtomicBool::new(false));

    let mut excluded_metrics_ports = BTreeSet::new();
    if let Some(controller_port) = extract_port_from_url(&args.url) {
        excluded_metrics_ports.insert(controller_port);
    }

    let metrics_store = Arc::new(MetricsStore::new());
    let metrics_config = MetricsScannerConfig::from_env(excluded_metrics_ports);
    let mut metrics_forwarder_config = MetricsForwarderConfig::from_env();
    metrics_forwarder_config.clamp_to_scan_interval(metrics_config.scan_interval);

    let processes = Arc::new(Mutex::new(Vec::<Child>::new()));
    let thread_pool = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));

    start_background_metrics_scanner(
        &thread_pool,
        &metrics_config,
        Arc::clone(&metrics_store),
        Arc::clone(&shutdown),
        args.node_id.clone(),
    );

    let node_id = args.node_id.clone();
    let client = match build_socket_client(
        &args.url,
        node_id.clone(),
        Arc::clone(&processes),
        Arc::clone(&thread_pool),
        Arc::clone(&shutdown),
        Arc::clone(&websocket_connected),
    ) {
        Ok(client) => client,
        Err(_) => return Ok(()),
    };

    set_application_log_client(Arc::clone(&client));

    info!(
        "Agent ({}) connected to controller at {}",
        node_id, args.url
    );

    start_background_metrics_forwarder(
        &thread_pool,
        &metrics_forwarder_config,
        Arc::clone(&metrics_store),
        Arc::clone(&client),
        Arc::clone(&shutdown),
        args.node_id.clone(),
    );

    spawn_shutdown_signal_thread(Arc::clone(&processes), Arc::clone(&shutdown));

    let mut disconnected_iterations = 0_u8;

    while !shutdown.load(Ordering::Acquire) {
        clean_threads(&thread_pool);
        reap_exited_processes(&processes);

        let metrics_snapshot = metrics_store.snapshot();
        let is_connected = websocket_connected.load(Ordering::Acquire);
        if is_connected {
            disconnected_iterations = 0;
        } else {
            disconnected_iterations = disconnected_iterations.saturating_add(1);
        }
        debug!(
            "Main loop is running... tracked_metrics_targets={}, completed_scan_rounds={}, websocket_connected={}, disconnected_iterations={}",
            metrics_snapshot.targets.len(),
            metrics_snapshot.scan_rounds_completed,
            is_connected,
            disconnected_iterations
        );

        if disconnected_iterations >= 2 {
            warn!(
                "WebSocket connection has been unavailable for {} consecutive main loop iterations, shutting down agent",
                disconnected_iterations
            );
            shutdown.store(true, Ordering::Release);
            shutdown_managed_processes(&processes);
            break;
        }

        thread::sleep(Duration::from_secs(30));

        if let Ok(client_lock) = client.lock() {
            let payload = json!({ "level": "info", "data": "[agent] I'm still running!" });
            if let Err(e) = client_lock.emit("process_output", payload) {
                error!("Failed to heartbeat: {}", e);
            }
        } else {
            error!("Failed to acquire lock on client");
        }
    }

    join_all_threads(&thread_pool);
    Ok(())
}

fn initialize_logging(
    log_level_filter: tracing::level_filters::LevelFilter,
) -> Result<(), Box<dyn std::error::Error>> {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(log_level_filter);

    let app_layer = ApplicationLoggingLayer {
        log_level: log_level_filter.into_level().unwrap_or(Level::INFO),
    }
    .with_filter(log_level_filter);

    let subscriber = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(app_layer);
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

fn start_background_metrics_scanner(
    thread_pool: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    metrics_config: &MetricsScannerConfig,
    metrics_store: Arc<MetricsStore>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) {
    match start_metrics_scanner(metrics_config.clone(), metrics_store, shutdown, node_id) {
        Ok(handle) => {
            match thread_pool.lock() {
                Ok(mut pool) => pool.push(handle),
                Err(e) => error!("Failed to acquire lock on thread_pool: {}", e),
            }
            info!(
                "Started metrics scanner for localhost ports {}-{} (excluded: {:?}, discovery every {:?}, active scrape every {:?})",
                metrics_config.port_start,
                metrics_config.port_end,
                metrics_config.excluded_ports,
                metrics_config.discovery_interval,
                metrics_config.scan_interval,
            );
        }
        Err(e) => warn!("Failed to start metrics scanner: {}", e),
    }
}

fn start_background_metrics_forwarder(
    thread_pool: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    metrics_forwarder_config: &MetricsForwarderConfig,
    metrics_store: Arc<MetricsStore>,
    client: Arc<Mutex<rust_socketio::client::Client>>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) {
    match start_metrics_forwarder(
        metrics_forwarder_config.clone(),
        metrics_store,
        client,
        shutdown,
        node_id,
    ) {
        Ok(handle) => match thread_pool.lock() {
            Ok(mut pool) => {
                pool.push(handle);
                info!(
                    "Started metrics forwarder using websocket event '{}' (poll every {:?}, force emit every {:?})",
                    metrics_forwarder_config.event_name,
                    metrics_forwarder_config.poll_interval,
                    metrics_forwarder_config.force_emit_interval,
                );
            }
            Err(e) => error!("Failed to acquire lock on thread_pool: {}", e),
        },
        Err(e) => warn!("Failed to start metrics forwarder: {}", e),
    }
}

fn build_socket_client(
    url: &str,
    node_id: String,
    processes: Arc<Mutex<Vec<Child>>>,
    thread_pool: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<AtomicBool>,
    websocket_connected: Arc<AtomicBool>,
) -> Result<Arc<Mutex<rust_socketio::client::Client>>, Box<dyn std::error::Error>> {
    let socket_id = Arc::new(RwLock::new(None));
    let socket_id_ref = Arc::clone(&socket_id);

    let client = ClientBuilder::new(url)
        .on("connect", {
            let node_id = node_id.clone();
            let websocket_connected = Arc::clone(&websocket_connected);
            move |_, socket| {
                websocket_connected.store(true, Ordering::Release);
                info!("Connected to controller");
                if let Err(e) = socket.emit("agent_ready", node_id.clone()) {
                    error!("Failed to emit agent_ready on connect: {}", e);
                }
            }
        })
        .on("disconnect", {
            let websocket_connected = Arc::clone(&websocket_connected);
            move |_, _| {
                websocket_connected.store(false, Ordering::Release);
                info!("Disconnected from controller");
            }
        })
        .on("close", {
            let websocket_connected = Arc::clone(&websocket_connected);
            move |_, _| {
                websocket_connected.store(false, Ordering::Release);
                info!("Closed WebSocket connection");
            }
        })
        .on("error", {
            let websocket_connected = Arc::clone(&websocket_connected);
            move |err, _| {
                websocket_connected.store(false, Ordering::Release);
                error!("Error: {:#?}", err);
            }
        })
        .on_with_ack("has_connected", {
            let socket_id_ref = Arc::clone(&socket_id_ref);
            let node_id = node_id.clone();
            let websocket_connected = Arc::clone(&websocket_connected);
            move |payload: Payload, s: RawClient, ack: i32| {
                let _ = s.ack(ack, "Ok".to_string());

                if let Payload::Text(values) = payload {
                    if let Some(socket_id) = values.first().and_then(|v| v.as_str()) {
                        match socket_id_ref.write() {
                            Ok(mut socket_id_lock) => {
                                websocket_connected.store(true, Ordering::Release);
                                *socket_id_lock = Some(socket_id.to_string());
                            }
                            Err(e) => {
                                error!("Failed to acquire lock on socket_id: {}", e);
                                return;
                            }
                        }

                        if let Err(e) = s.emit("agent_ready", node_id.clone()) {
                            error!("Failed to emit agent_ready event: {}", e);
                        }
                        thread::sleep(Duration::from_secs(1));

                        emit_log(
                            &s,
                            "info",
                            true,
                            &format!("WebSocket connected with id: {socket_id} for {node_id}"),
                        );
                    }
                }
            }
        })
        .on("update_network_conditions", move |payload, socket| {
            handle_update_network_conditions(payload, socket);
        })
        .on("update_route_weights", move |payload, socket| {
            handle_update_route_weights(payload, socket);
        })
        .on("start_process", {
            let processes = Arc::clone(&processes);
            let thread_pool = Arc::clone(&thread_pool);
            move |payload, socket| {
                handle_start_process_event(
                    payload,
                    socket,
                    Arc::clone(&processes),
                    Arc::clone(&thread_pool),
                );
            }
        })
        .on("stop_process", {
            let processes = Arc::clone(&processes);
            let thread_pool = Arc::clone(&thread_pool);
            move |_, socket| {
                handle_stop_process_event(socket, Arc::clone(&processes), Arc::clone(&thread_pool));
            }
        })
        .on("shutdown_agent", {
            let processes = Arc::clone(&processes);
            let shutdown = Arc::clone(&shutdown);
            move |_, socket| {
                emit_log(&socket, "info", true, "Shutting down agent on controller request");
                shutdown.store(true, Ordering::Release);
                shutdown_managed_processes(&processes);
                std::process::exit(0);
            }
        })
        .on_with_ack("curl", {
            let thread_pool = Arc::clone(&thread_pool);
            move |payload, socket, ack| {
                handle_curl_ack_event(payload, socket, ack, Arc::clone(&thread_pool));
            }
        })
        .connect();

    match client {
        Ok(s) => Ok(Arc::new(Mutex::new(s))),
        Err(err) => {
            error!("Failed to connect WebSocket: {:#?}", err);
            Err(Box::<dyn std::error::Error>::from(err))
        }
    }
}

fn handle_update_network_conditions(payload: Payload, socket: RawClient) {
    if let Payload::Text(data) = payload {
        if data.len() != 1 {
            emit_log(
                &socket,
                "error",
                true,
                "Invalid payload format: expected a single object",
            );
            return;
        }
        let serde_json::Value::Object(json_data) = data[0].clone() else {
            emit_log(&socket, "error", true, "Failed to parse JSON payload");
            return;
        };

        let bandwidth_mbit = json_data["bandwidth"].as_str().unwrap_or("1000mbit");
        let latency_ms = json_data["latency"].as_str().unwrap_or("0ms");
        let loss_percent = json_data["loss"].as_str().unwrap_or("0%");
        let htb_explicit_limits = json_data["htb_explicit_limits"].as_bool().unwrap_or(false);
        let interface = json_data["interface"].as_str().unwrap_or("");
        let interface_ip = json_data["interface_ip"].as_str().unwrap_or("");

        let interfaces = match resolve_network_condition_interfaces(
            (!interface.trim().is_empty()).then_some(interface),
            (!interface_ip.trim().is_empty()).then_some(interface_ip),
        ) {
            Ok(interfaces) => interfaces,
            Err(e) => {
                emit_log(
                    &socket,
                    "error",
                    true,
                    &format!(
                        "Failed to resolve network-condition interface hint '{}' (ip '{}'): {e}",
                        interface,
                        interface_ip,
                    ),
                );
                return;
            }
        };

        if (!interface.trim().is_empty() || !interface_ip.trim().is_empty()) && interfaces.is_empty() {
            emit_log(
                &socket,
                "error",
                true,
                &format!(
                    "Failed to resolve network-condition interface hint '{}' (ip '{}') to a local device",
                    interface,
                    interface_ip,
                ),
            );
            return;
        }

        if !interface.trim().is_empty() || !interface_ip.trim().is_empty() {
            emit_log(
                &socket,
                "info",
                true,
                &format!(
                    "Resolved network-condition interface hint '{}' (ip '{}') to {:?}",
                    interface,
                    interface_ip,
                    interfaces,
                ),
            );
        }

        match set_network_conditions(
            &interfaces,
            bandwidth_mbit,
            latency_ms,
            loss_percent,
            htb_explicit_limits,
        ) {
            Ok(result) => {
                for line in result {
                    emit_log(&socket, "info", true, &line);
                }
            }
            Err(e) => {
                emit_log(
                    &socket,
                    "error",
                    true,
                    &format!("Failed to set network conditions: {e}"),
                );
            }
        }
    } else {
        emit_log(
            &socket,
            "error",
            true,
            "Invalid payload for update_network_conditions",
        );
    }
}

fn handle_start_process_event(
    payload: Payload,
    socket: RawClient,
    processes: Arc<Mutex<Vec<Child>>>,
    thread_pool: Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    if let Payload::Text(data) = payload {
        let mut proc_args: Vec<String> = data
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        proc_args = proc_args
            .iter()
            .flat_map(|s| s.split_whitespace().map(String::from))
            .collect();
        if !proc_args.is_empty() {
            let command = proc_args.join(" ");
            emit_log(
                &socket.clone(),
                "info",
                true,
                &format!("Received start_process command: {command}"),
            );
            let process_clone = Arc::clone(&processes);
            let socket_clone = socket.clone();
            match thread_pool.lock() {
                Ok(mut pool) => {
                    pool.push(
                        thread::Builder::new()
                            .name("start_process_thread".to_string())
                            .spawn(move || {
                                start_process(process_clone, proc_args, socket_clone);
                            })
                            .expect("Failed to spawn start_process_thread"),
                    );
                }
                Err(e) => {
                    error!("Failed to acquire lock on thread_pool: {}", e);
                }
            };
        } else {
            emit_log(
                &socket,
                "error",
                true,
                "Received empty start_process command",
            );
        }
    }
}

fn handle_update_route_weights(payload: Payload, socket: RawClient) {
    let request = match payload {
        Payload::Text(data) => {
            if data.len() != 1 {
                emit_log(
                    &socket,
                    "error",
                    true,
                    "Invalid payload format: expected a single object",
                );
                return;
            }

            match serde_json::from_value::<RouteUpdateRequest>(data[0].clone()) {
                Ok(request) => request,
                Err(e) => {
                    emit_log(
                        &socket,
                        "error",
                        true,
                        &format!("Failed to parse route update payload: {e}"),
                    );
                    return;
                }
            }
        }
        _ => {
            emit_log(
                &socket,
                "error",
                true,
                "Invalid payload for update_route_weights",
            );
            return;
        }
    };

    match apply_route_update(&request) {
        Ok(result) => {
            let level = if result.applied { "info" } else { "warn" };
            emit_log(
                &socket,
                level,
                true,
                &format!("Route update [{}]: {}", result.route, result.detail),
            );

            let _ = socket.emit(
                "route_update_result",
                json!({
                    "status": "ok",
                    "applied": result.applied,
                    "route": result.route,
                    "detail": result.detail,
                }),
            );
        }
        Err(e) => {
            let message = format!("Failed route update [{}]: {}", request.route, e);
            emit_log(&socket, "error", true, &message);
            let _ = socket.emit(
                "route_update_result",
                json!({
                    "status": "error",
                    "applied": false,
                    "route": request.route,
                    "detail": e.to_string(),
                }),
            );
        }
    }
}

fn handle_stop_process_event(
    socket: RawClient,
    processes: Arc<Mutex<Vec<Child>>>,
    thread_pool: Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    let process_clone = Arc::clone(&processes);
    let socket_clone = socket.clone();
    match thread_pool.lock() {
        Ok(mut pool) => {
            pool.push(
                thread::Builder::new()
                    .name("stop_process_thread".to_string())
                    .spawn(move || {
                        stop_process(process_clone, socket_clone);
                    })
                    .expect("Failed to spawn stop_process_thread"),
            );
        }
        Err(e) => {
            error!("Failed to acquire lock on thread_pool: {}", e);
        }
    }
}

fn handle_curl_ack_event(
    payload: Payload,
    socket: RawClient,
    ack: i32,
    thread_pool: Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    let socket_clone = socket.clone();
    match thread_pool.lock() {
        Ok(mut pool) => {
            pool.push(
                thread::Builder::new()
                    .name("curl_request_thread".to_string())
                    .spawn(move || {
                        handle_curl_event(payload, socket_clone, ack);
                    })
                    .expect("Failed to spawn curl_request_thread"),
            );
        }
        Err(e) => {
            error!("Failed to acquire lock on thread_pool: {}", e);
            let _ = socket.ack(
                ack,
                json!({
                    "status": "error",
                    "message": format!("Failed to acquire thread pool lock: {e}"),
                }),
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn spawn_shutdown_signal_thread(processes: Arc<Mutex<Vec<Child>>>, shutdown: Arc<AtomicBool>) {
    let _signal_thread = std::thread::Builder::new()
        .name("shutdown_signal_thread".into())
        .spawn(move || {
            let mut signals =
                Signals::new(TERM_SIGNALS).expect("failed to register signal handlers");
            #[cfg(not(windows))]
            signals
                .add_signal(libc::SIGHUP)
                .expect("failed to add SIGHUP");

            if let Some(sig) = signals.forever().next() {
                info!("Received shutdown signal: {}", sig);
                shutdown.store(true, Ordering::Release);
                shutdown_managed_processes(&processes);
            }
        })
        .expect("failed to spawn shutdown signal thread");
}

#[cfg(not(target_os = "linux"))]
fn spawn_shutdown_signal_thread(_processes: Arc<Mutex<Vec<Child>>>, _shutdown: Arc<AtomicBool>) {}

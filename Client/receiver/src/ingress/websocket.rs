use crate::clock::{ClockDomain, ClockOffsetSample, ClockSampleTrust, ClockSourceKey};
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use metrics::get_metrics;
use pcf::types::PCF_MAGIC;
use prometheus::IntGauge;
use rbase64;
use rust_socketio::{client::Client, ClientBuilder, Payload, RawClient};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;
use tracing::{debug, error, info, warn};
use webrtc::ice::candidate::candidate_base::unmarshal_candidate;
use webrtc::ice::candidate::Candidate;

use super::{dash::DashIngress, webrtc::WebRTCIngress};

const WEBSOCKET_CONNECTS_TOTAL_HELP: &str =
    "Total number of successful receiver WebSocket signaling connections";
const WEBSOCKET_DISCONNECTS_TOTAL_HELP: &str =
    "Total number of receiver WebSocket disconnect or close events after a successful connection";
const WEBSOCKET_ERRORS_TOTAL_HELP: &str =
    "Total number of receiver WebSocket signaling errors, including connection setup failures";
const WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT: u64 = 5;
const WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_INTERVAL: Duration = Duration::from_millis(100);
const WEBSOCKET_CLOCK_SYNC_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW: Duration =
    Duration::from_millis(WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT * 100 + 250);

struct WebSocketRuntimeMetrics {
    connects_total: IntGauge,
    disconnects_total: IntGauge,
    errors_total: IntGauge,
    connected: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedClockSyncResponse {
    server_instance_id: String,
    local_send_us: u64,
    remote_receive_us: u64,
    remote_send_us: u64,
    sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedClockSyncSample {
    server_instance_id: String,
    local_send_us: u64,
    local_receive_us: u64,
    remote_receive_us: u64,
    remote_send_us: u64,
    sequence: Option<u64>,
}

impl ObservedClockSyncSample {
    fn total_rtt_us(&self) -> u64 {
        self.local_receive_us.saturating_sub(self.local_send_us)
    }

    fn offset_sample(&self) -> ClockOffsetSample {
        ClockOffsetSample {
            remote_now_us: self.remote_send_us,
            local_send_us: self.local_send_us,
            local_receive_us: self.local_receive_us,
            server_wait_us: Some(self.remote_send_us.saturating_sub(self.remote_receive_us)),
        }
    }
}

#[derive(Default)]
struct ClockSyncStartupState {
    first_startup_response_at: Option<Instant>,
    startup_sequences_seen: HashSet<u64>,
    best_sample: Option<ObservedClockSyncSample>,
    startup_applied: bool,
}

impl ClockSyncStartupState {
    fn select_sample_to_apply(
        &mut self,
        sample: ObservedClockSyncSample,
        observed_at: Instant,
    ) -> Option<ObservedClockSyncSample> {
        if self.startup_applied {
            return Some(sample);
        }

        let Some(sequence) = sample.sequence else {
            self.startup_applied = true;
            return self.best_sample.take().or(Some(sample));
        };

        if sequence >= WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT {
            self.startup_applied = true;
            return self.best_sample.take().or(Some(sample));
        }

        let first_response_at = self.first_startup_response_at.get_or_insert(observed_at);
        self.startup_sequences_seen.insert(sequence);

        let should_replace_best = match self.best_sample.as_ref() {
            Some(current_best) => sample.total_rtt_us() < current_best.total_rtt_us(),
            None => true,
        };
        if should_replace_best {
            self.best_sample = Some(sample);
        }

        let observed_last_startup_probe = sequence + 1 >= WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT;
        if observed_last_startup_probe
            || self.startup_sequences_seen.len() as u64 >= WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT
            || observed_at.saturating_duration_since(*first_response_at)
                >= WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW
        {
            self.startup_applied = true;
            return self.best_sample.take();
        }

        None
    }

    fn take_best_sample_if_startup_window_elapsed(
        &mut self,
        observed_at: Instant,
    ) -> Option<ObservedClockSyncSample> {
        if self.startup_applied {
            return None;
        }

        let first_response_at = self.first_startup_response_at?;
        if observed_at.saturating_duration_since(first_response_at)
            < WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW
        {
            return None;
        }

        self.startup_applied = true;
        self.best_sample.take()
    }
}

impl WebSocketRuntimeMetrics {
    fn new() -> Self {
        let metrics = get_metrics();

        Self {
            connects_total: metrics
                .get_or_create_gauge("websocket_connects_total", WEBSOCKET_CONNECTS_TOTAL_HELP)
                .expect("websocket_connects_total"),
            disconnects_total: metrics
                .get_or_create_gauge(
                    "websocket_disconnects_total",
                    WEBSOCKET_DISCONNECTS_TOTAL_HELP,
                )
                .expect("websocket_disconnects_total"),
            errors_total: metrics
                .get_or_create_gauge("websocket_errors_total", WEBSOCKET_ERRORS_TOTAL_HELP)
                .expect("websocket_errors_total"),
            connected: AtomicBool::new(false),
        }
    }

    fn record_connected(&self) {
        if !self.connected.swap(true, Ordering::SeqCst) {
            self.connects_total.inc();
        }
    }

    fn record_disconnected(&self) {
        if self.connected.swap(false, Ordering::SeqCst) {
            self.disconnects_total.inc();
        }
    }

    fn record_error(&self) {
        self.errors_total.inc();
    }

    fn reset(&self) {
        self.connects_total.set(0);
        self.disconnects_total.set(0);
        self.errors_total.set(0);
        self.connected.store(false, Ordering::SeqCst);
    }
}

pub struct WebSocketIngress {
    url: String,
    socket: Arc<Mutex<Option<Client>>>,
    socket_id: Arc<RwLock<Option<String>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    pub runtime: Arc<Mutex<Option<Runtime>>>,
    webrtc_ingress: Arc<WebRTCIngress>,
    dash_ingress: Arc<DashIngress>,
    runtime_metrics: Arc<WebSocketRuntimeMetrics>,
    clock_source_id: Arc<Mutex<Option<String>>>,
}
crate::log_drop!(WebSocketIngress);

impl WebSocketIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let url = stream_manager.websocket_url.read().unwrap().clone();
        if url.is_none() {
            error!("WebSocket URL is empty");
            return;
        }

        let runtime = Arc::clone(&processing_pipeline.runtime);
        let runtime_metrics = Arc::new(WebSocketRuntimeMetrics::new());
        runtime_metrics.reset();

        // Get a reference to the WebRTCIngress
        let webrtc_ingress = {
            // The `StreamManager` now has webrtc_ingress set by WebRTCIngress::initialize
            match stream_manager.webrtc_ingress.read().unwrap().as_ref() {
                Some(i) => i.clone(),
                None => {
                    error!("WebRTCIngress not found, did you call WebRTCIngress::initialize()?");
                    return;
                }
            }
        };

        // Get a reference to the DashIngress
        let dash_ingress = {
            // The `StreamManager` now has dash_ingress set by DashIngress::initialize
            match stream_manager.dash_ingress.read().unwrap().as_ref() {
                Some(i) => i.clone(),
                None => {
                    error!("DashIngress not found, did you call DashIngress::initialize()?");
                    return;
                }
            }
        };

        let ingress = Arc::new(Self {
            url: url.unwrap(),
            socket: Arc::new(Mutex::new(None)),
            socket_id: Arc::new(RwLock::new(None)),
            processing_pipeline,
            runtime,
            webrtc_ingress,
            dash_ingress,
            runtime_metrics,
            clock_source_id: Arc::new(Mutex::new(None)),
        });

        ingress.connect();

        stream_manager.set_websocket_ingress(ingress)
    }

    pub fn stop(&self) {
        // close the socket-io client (drops its background thread)
        if let Some(client) = self.socket.lock().unwrap().take() {
            let _ = client.disconnect();
            self.runtime_metrics.record_disconnected();
        }
    }

    fn process_payload(
        payload: Payload,
        processing_pipeline: Arc<ProcessingPipeline>,
        clock_source: ClockSourceKey,
    ) {
        let Payload::Binary(bytes) = payload else {
            warn!("Unsupported payload format");
            return;
        };

        // base64 decode using rbase64
        let bytes_str = match std::str::from_utf8(&bytes) {
            Ok(v) => v,
            Err(e) => {
                warn!("Invalid UTF-8 sequence: {}", e);
                return;
            }
        };
        let bytes_decoded = match rbase64::decode(bytes_str) {
            Ok(decoded) => decoded,
            Err(err) => {
                warn!("Failed to decode payload: {}", err);
                return;
            }
        };

        if !bytes_decoded.starts_with(PCF_MAGIC) {
            warn!(
                "Received non-PCF WebSocket frame with {} bytes",
                bytes_decoded.len()
            );
            return;
        }

        debug!(
            "Received direct PCF frame with {} bytes",
            bytes_decoded.len()
        );
        processing_pipeline.ingest_data_for_source(
            clock_source,
            "client_0_0".to_string(),
            0,
            0,
            0,
            bytes_decoded,
        );
    }

    pub fn get_socket(&self) -> Arc<Mutex<Option<Client>>> {
        Arc::clone(&self.socket)
    }

    fn current_time_us() -> Option<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
    }

    fn parse_clock_sync_response(payload: Payload) -> Option<ParsedClockSyncResponse> {
        let Payload::Text(values) = payload else {
            return None;
        };
        let value = values.first()?;
        let server_instance_id = value
            .get("serverInstanceId")
            .or_else(|| value.get("server_instance_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let local_send_us = value
            .get("localSendUs")
            .or_else(|| value.get("local_send_us"))
            .and_then(Value::as_u64)?;
        let remote_receive_us = value
            .get("remoteReceiveUs")
            .or_else(|| value.get("remote_receive_us"))
            .and_then(Value::as_u64)?;
        let remote_send_us = value
            .get("remoteSendUs")
            .or_else(|| value.get("remote_send_us"))
            .and_then(Value::as_u64)?;
        let sequence = value.get("sequence").and_then(Value::as_u64);

        Some(ParsedClockSyncResponse {
            server_instance_id,
            local_send_us,
            remote_receive_us,
            remote_send_us,
            sequence,
        })
    }

    fn start_clock_sync_loop(socket: Client) {
        let _ = std::thread::Builder::new()
            .name("websocket_clock_sync".to_string())
            .spawn(move || {
                let mut sequence = 0_u64;
                for _ in 0..WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT {
                    if !Self::emit_clock_sync_request(&socket, sequence) {
                        return;
                    }
                    sequence = sequence.saturating_add(1);
                    std::thread::sleep(WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_INTERVAL);
                }

                loop {
                    std::thread::sleep(WEBSOCKET_CLOCK_SYNC_REFRESH_INTERVAL);
                    if !Self::emit_clock_sync_request(&socket, sequence) {
                        break;
                    }
                    sequence = sequence.saturating_add(1);
                }
            });
    }

    fn apply_clock_sync_sample(
        clock_pipeline: &Arc<ProcessingPipeline>,
        websocket_clock_source_id: &Arc<Mutex<Option<String>>>,
        webrtc_clock_ingress: &Arc<WebRTCIngress>,
        observed_sample: ObservedClockSyncSample,
    ) {
        *websocket_clock_source_id.lock().unwrap() =
            Some(observed_sample.server_instance_id.clone());
        webrtc_clock_ingress.set_clock_source_id(Some(observed_sample.server_instance_id.clone()));

        let sample = observed_sample.offset_sample();
        let _ = clock_pipeline.observe_clock_offset_sample(
            ClockSourceKey::with_server_id(
                ClockDomain::WebSocket,
                observed_sample.server_instance_id.clone(),
            ),
            ClockSampleTrust::HighRtt,
            sample,
        );
        let _ = clock_pipeline.observe_clock_offset_sample(
            ClockSourceKey::with_server_id(ClockDomain::WebRtc, observed_sample.server_instance_id),
            ClockSampleTrust::HighRtt,
            sample,
        );
    }

    fn spawn_clock_sync_startup_flush(
        clock_sync_startup_state: Arc<Mutex<ClockSyncStartupState>>,
        clock_pipeline: Arc<ProcessingPipeline>,
        websocket_clock_source_id: Arc<Mutex<Option<String>>>,
        webrtc_clock_ingress: Arc<WebRTCIngress>,
    ) {
        let _ = std::thread::Builder::new()
            .name("websocket_clock_sync_startup_flush".to_string())
            .spawn(move || {
                std::thread::sleep(WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW);

                let sample_to_apply = {
                    let mut startup_state = clock_sync_startup_state.lock().unwrap();
                    startup_state.take_best_sample_if_startup_window_elapsed(Instant::now())
                };
                if let Some(sample_to_apply) = sample_to_apply {
                    Self::apply_clock_sync_sample(
                        &clock_pipeline,
                        &websocket_clock_source_id,
                        &webrtc_clock_ingress,
                        sample_to_apply,
                    );
                }
            });
    }

    fn emit_clock_sync_request(socket: &Client, sequence: u64) -> bool {
        let Some(local_send_us) = Self::current_time_us() else {
            return true;
        };
        let payload = json!({
            "localSendUs": local_send_us,
            "sequence": sequence,
        });
        match socket.emit("clock_sync_request", payload) {
            Ok(()) => true,
            Err(err) => {
                debug!("Stopping WebSocket clock sync loop: {:?}", err);
                false
            }
        }
    }

    pub fn connect(&self) {
        let socket_id_ref = Arc::clone(&self.socket_id);
        let disconnect_metrics = Arc::clone(&self.runtime_metrics);
        let close_metrics = Arc::clone(&self.runtime_metrics);
        let error_metrics = Arc::clone(&self.runtime_metrics);
        let connect_metrics = Arc::clone(&self.runtime_metrics);
        let clock_pipeline = Arc::clone(&self.processing_pipeline);
        let websocket_clock_source_id = Arc::clone(&self.clock_source_id);
        let webrtc_clock_ingress = Arc::clone(&self.webrtc_ingress);
        let clock_sync_startup_state = Arc::new(Mutex::new(ClockSyncStartupState::default()));

        let socket = match ClientBuilder::new(&self.url)
            .namespace("/")
            .reconnect_on_disconnect(false)
            // Some basic logging
            .on("disconnect", move |_, _| {
                disconnect_metrics.record_disconnected();
                info!("Disconnected from WebSocket server")
            })
            .on("close", move |_, _| {
                close_metrics.record_disconnected();
                info!("Closed WebSocket connection")
            })
            .on("error", move |err, _| {
                error_metrics.record_error();
                error!("Error: {:#?}", err)
            })
            .on("clock_sync_response", {
                let clock_sync_startup_state = Arc::clone(&clock_sync_startup_state);
                let clock_pipeline = Arc::clone(&clock_pipeline);
                let websocket_clock_source_id = Arc::clone(&websocket_clock_source_id);
                let webrtc_clock_ingress = Arc::clone(&webrtc_clock_ingress);
                move |payload, _| {
                    let Some(parsed_response) = Self::parse_clock_sync_response(payload) else {
                        return;
                    };
                    let Some(local_receive_us) = Self::current_time_us() else {
                        return;
                    };
                    let observed_sample = ObservedClockSyncSample {
                        server_instance_id: parsed_response.server_instance_id,
                        local_send_us: parsed_response.local_send_us,
                        local_receive_us,
                        remote_receive_us: parsed_response.remote_receive_us,
                        remote_send_us: parsed_response.remote_send_us,
                        sequence: parsed_response.sequence,
                    };

                    let (sample_to_apply, schedule_startup_flush) = {
                        let mut startup_state = clock_sync_startup_state.lock().unwrap();
                        let should_schedule_startup_flush =
                            startup_state.first_startup_response_at.is_none();
                        let sample_to_apply =
                            startup_state.select_sample_to_apply(observed_sample, Instant::now());
                        (
                            sample_to_apply,
                            should_schedule_startup_flush && !startup_state.startup_applied,
                        )
                    };
                    if schedule_startup_flush {
                        Self::spawn_clock_sync_startup_flush(
                            Arc::clone(&clock_sync_startup_state),
                            Arc::clone(&clock_pipeline),
                            Arc::clone(&websocket_clock_source_id),
                            Arc::clone(&webrtc_clock_ingress),
                        );
                    }
                    if let Some(sample_to_apply) = sample_to_apply {
                        Self::apply_clock_sync_sample(
                            &clock_pipeline,
                            &websocket_clock_source_id,
                            &webrtc_clock_ingress,
                            sample_to_apply,
                        );
                    }
                }
            })
            // We listen for the "has_connected" event to get the socket id
            // This is a custom event that is emitted by the server to get the socket id
            // To resolve an issue with the Rust socket.io server library, we acknowledge the event. (This is not needed for other events)
            // See the comment in the server code for more information
            .on_with_ack("has_connected", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress = Arc::clone(&self.webrtc_ingress);
                let socket_id_ref = Arc::clone(&socket_id_ref);
                let socket_ref = Arc::clone(&self.socket);
                let connect_metrics = Arc::clone(&connect_metrics);
                move |payload: Payload, s: RawClient, ack: i32| {
                    // Acknowledge the event
                    let _ = s.ack(ack, "Ok".to_string());

                    // Extract the socket id from the payload
                    let Payload::Text(values) = payload else {
                        return;
                    };

                    // The payload should contain at least 1 value: the socket id
                    if values.is_empty() {
                        return;
                    }

                    // Get the socket id
                    let socket_id = values[0].as_str().unwrap_or("").to_string();
                    info!("WebSocket connected with id: {:#?}", socket_id);
                    connect_metrics.record_connected();

                    // Store the socket id
                    let mut socket_id_lock = socket_id_ref.write().unwrap();
                    *socket_id_lock = Some(socket_id.clone().to_string());

                    // Now that we are connected to the server, let's create our WebRTC offer.
                    // We must do this in a separate task (async).
                    let webrtc_ingress_clone = webrtc_ingress.clone();
                    if let Some(rt) = runtime_clone.lock().unwrap().as_ref() {
                        let local_sdp = rt.block_on(webrtc_ingress_clone.create_offer(&socket_ref));
                        match local_sdp {
                            Ok(local_sdp) => {
                                let offer_payload = serde_json::json!({
                                    "sdp": local_sdp,
                                    "clientId": socket_id.to_string()
                                });
                                if let Err(e) = s.emit::<&str, Value>("webrtc_offer", offer_payload)
                                {
                                    error!("Failed to emit webrtc_offer: {:?}", e);
                                }
                                //info!("Local SDP: {:#?}", local_sdp);
                            }
                            Err(e) => {
                                error!("Failed to create WebRTC offer: {}", e);
                            }
                        }
                    }
                }
            })
            .on_with_ack("webrtc_answer", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress_clone = Arc::clone(&self.webrtc_ingress);
                let socket_id_ref = Arc::clone(&socket_id_ref);
                move |payload: Payload, s: RawClient, ack: i32| {
                    let Payload::Text(values) = payload else {
                        warn!("Got webrtc_answer in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }

                    let serde_json::Value::Object(json_val) = values[0].clone() else {
                        warn!("Invalid payload format: expected an object");
                        return;
                    };

                    debug!("Received WebRTC answer from server");

                    let client_id = json_val["clientId"].as_str().unwrap_or("").to_string();
                    let socket_id_binding = socket_id_ref.read().unwrap();
                    let socket_id = socket_id_binding.as_deref().unwrap_or("unknown");
                    if client_id != socket_id {
                        warn!(
                            "Ignoring WebRTC answer: client id ({}) does not match socket id ({})",
                            client_id, socket_id
                        );
                        return;
                    }

                    let sdp = json_val["sdp"].as_str().unwrap_or("").to_string();
                    if sdp.is_empty() {
                        warn!("Ignoring WebRTC answer: empty SDP");
                        return;
                    }

                    if let Some(rt) = runtime_clone.lock().unwrap().as_ref() {
                        if let Err(e) = rt.block_on(webrtc_ingress_clone.handle_answer(sdp)) {
                            error!("Error handling WebRTC answer: {}", e);
                        }
                    }

                    //info!("WebRTC answer handled");

                    // Acknowledge the event
                    let _ = s.ack(ack, "Ok".to_string());
                }
            })
            .on("webrtc_ice_candidate", {
                let runtime_clone = Arc::clone(&self.runtime);
                let webrtc_ingress_clone = Arc::clone(&self.webrtc_ingress);
                let url = self.url.clone();
                move |payload: Payload, _s: RawClient| {
                    let Payload::Text(values) = payload else {
                        warn!("Got webrtc_ice_candidate in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }

                    let serde_json::Value::Object(json_val) = values[0].clone() else {
                        warn!("Invalid payload format: expected an object");
                        return;
                    };

                    //info!("Received ICE candidate: {:#?}", json_val);

                    // This is a JSON with {candidate, sdpMid, sdpMLineIndex}
                    let candidate = json_val["candidate"].as_str().unwrap_or("").to_string();
                    let sdp_mid = json_val["sdpMid"].as_str().map(|s| s.to_string());
                    let sdp_mline_index = json_val["sdpMLineIndex"].as_u64().map(|u| u as u16);

                    let candiate_clone = candidate.clone();
                    let candidate_value = match candiate_clone.strip_prefix("candidate:") {
                        Some(s) => s,
                        None => candiate_clone.as_str(),
                    };

                    let ice_candidate = if !candidate_value.is_empty() {
                        unmarshal_candidate(candidate_value)
                    } else {
                        warn!("Invalid ICE candidate: empty candidate");
                        return;
                    };

                    // If an error occurred, the ICE candidate is invalid
                    if ice_candidate.is_err() {
                        warn!("Invalid ICE candidate: {}", ice_candidate.err().unwrap());
                        return;
                    }
                    let ice_candidate = ice_candidate.unwrap();

                    // &self.url contains an url such as http://13.0.1.2:3001, extract the address
                    let mut expected_address = url.split(":").nth(1).unwrap_or("").to_string();
                    // Remove the leading "//"
                    expected_address = expected_address
                        .strip_prefix("//")
                        .unwrap_or("")
                        .to_string();

                    let ice_address = ice_candidate.address();

                    // TODO: instead of just using the server address, we should also support a list of allowed addresses
                    if ice_address.is_empty() || ice_address != expected_address {
                        // debug!("Invalid ICE candidate: address ({}) does not match expected address ({})", ice_address, expected_address);
                        debug!("Ignoring ICE candidate: {}", ice_candidate);
                        return;
                    }

                    if let Some(rt) = runtime_clone.lock().unwrap().as_ref() {
                        if let Err(e) = rt.block_on(webrtc_ingress_clone.handle_ice_candidate(
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                        )) {
                            error!("Error handling ICE candidate: {}", e);
                        }
                    }

                    debug!("ICE candidate handled");
                }
            })
            .on_with_ack("frame:broadcast:ack", {
                let processing_pipeline = Arc::clone(&self.processing_pipeline);
                let clock_source_id = Arc::clone(&self.clock_source_id);
                move |payload: Payload, s: RawClient, ack: i32| {
                    let _ = s.ack(ack, "Ok".to_string());
                    debug!("Received frame broadcast with ack");
                    let clock_source = clock_source_id
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|server_instance_id| {
                            ClockSourceKey::with_server_id(
                                ClockDomain::WebSocket,
                                server_instance_id,
                            )
                        })
                        .unwrap_or_else(|| ClockSourceKey::for_transport(ClockDomain::WebSocket));
                    WebSocketIngress::process_payload(
                        payload,
                        Arc::clone(&processing_pipeline),
                        clock_source,
                    );
                }
            })
            .on("frame:broadcast", {
                let processing_pipeline = Arc::clone(&self.processing_pipeline);
                let clock_source_id = Arc::clone(&self.clock_source_id);
                move |payload: Payload, _s: RawClient| {
                    debug!("Received frame broadcast without ack");
                    let clock_source = clock_source_id
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|server_instance_id| {
                            ClockSourceKey::with_server_id(
                                ClockDomain::WebSocket,
                                server_instance_id,
                            )
                        })
                        .unwrap_or_else(|| ClockSourceKey::for_transport(ClockDomain::WebSocket));
                    WebSocketIngress::process_payload(
                        payload,
                        Arc::clone(&processing_pipeline),
                        clock_source,
                    );
                }
            })
            .on("mpd::group_id", {
                let dash_ingress = Arc::clone(&self.dash_ingress);
                move |payload: Payload, _s: RawClient| {
                    let Payload::Text(values) = payload else {
                        warn!("Got mpd::group_id in unrecognized format");
                        return;
                    };

                    if values.len() != 1 {
                        warn!("Invalid payload format: expected a single object");
                        return;
                    }

                    let group_id = values[0].as_str().unwrap_or("").to_string();
                    info!("Received MPD group id: {:#?}", group_id);

                    dash_ingress.spawn_group(group_id.clone());
                }
            })
            .connect()
        {
            Ok(s) => s,
            Err(err) => {
                self.runtime_metrics.record_error();
                error!("Failed to connect WebSocket: {:#?}", err);
                return;
            }
        };

        Self::start_clock_sync_loop(socket.clone());

        // Store the socket
        let mut socket_lock = self.socket.lock().unwrap();
        *socket_lock = Some(socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn ensure_metrics_initialized() {
        if catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok() {
            return;
        }

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = metrics::MetricsBuilder::new()
                .add_label("mode", "client-test")
                .build();
        }));

        assert!(
            catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok(),
            "failed to initialize global metrics for receiver tests"
        );
    }

    #[test]
    fn websocket_runtime_metrics_record_connect_disconnect_and_errors() {
        ensure_metrics_initialized();

        let metrics = WebSocketRuntimeMetrics::new();
        metrics.reset();
        metrics.record_connected();
        metrics.record_connected();
        metrics.record_error();
        metrics.record_disconnected();
        metrics.record_disconnected();

        let registry = get_metrics();
        let connects_total = registry
            .get_or_create_gauge("websocket_connects_total", WEBSOCKET_CONNECTS_TOTAL_HELP)
            .unwrap();
        let disconnects_total = registry
            .get_or_create_gauge(
                "websocket_disconnects_total",
                WEBSOCKET_DISCONNECTS_TOTAL_HELP,
            )
            .unwrap();
        let errors_total = registry
            .get_or_create_gauge("websocket_errors_total", WEBSOCKET_ERRORS_TOTAL_HELP)
            .unwrap();

        assert_eq!(connects_total.get(), 1);
        assert_eq!(disconnects_total.get(), 1);
        assert_eq!(errors_total.get(), 1);
    }

    #[test]
    fn parse_clock_sync_response_accepts_camel_case_payload() {
        let payload = Payload::Text(vec![json!({
            "serverInstanceId": "server-a",
            "localSendUs": 100,
            "remoteReceiveUs": 140,
            "remoteSendUs": 145,
            "sequence": 3,
        })]);

        let parsed = WebSocketIngress::parse_clock_sync_response(payload)
            .expect("clock sync response should parse");

        assert_eq!(
            parsed,
            ParsedClockSyncResponse {
                server_instance_id: "server-a".to_string(),
                local_send_us: 100,
                remote_receive_us: 140,
                remote_send_us: 145,
                sequence: Some(3),
            }
        );
    }

    #[test]
    fn clock_sync_startup_prefers_lowest_rtt_sample_from_burst() {
        let mut state = ClockSyncStartupState::default();
        let base = Instant::now();
        let mut applied = None;

        for (sequence, local_send_us, local_receive_us) in [
            (0, 1_000, 1_090),
            (1, 2_000, 2_040),
            (2, 3_000, 3_060),
            (3, 4_000, 4_120),
            (4, 5_000, 5_080),
        ] {
            let sample = ObservedClockSyncSample {
                server_instance_id: "server-a".to_string(),
                local_send_us,
                local_receive_us,
                remote_receive_us: local_send_us + 10,
                remote_send_us: local_send_us + 15,
                sequence: Some(sequence),
            };
            let observed_at = base + Duration::from_millis(sequence * 100);
            if let Some(sample) = state.select_sample_to_apply(sample, observed_at) {
                applied = Some(sample);
            }
        }

        let applied = applied.expect("startup burst should yield a selected sample");
        assert_eq!(applied.sequence, Some(1));
        assert_eq!(applied.total_rtt_us(), 40);
    }

    #[test]
    fn clock_sync_startup_uses_buffered_best_sample_when_periodic_probe_arrives() {
        let mut state = ClockSyncStartupState::default();
        let base = Instant::now();

        let startup = ObservedClockSyncSample {
            server_instance_id: "server-a".to_string(),
            local_send_us: 1_000,
            local_receive_us: 1_050,
            remote_receive_us: 1_010,
            remote_send_us: 1_015,
            sequence: Some(0),
        };
        assert!(state.select_sample_to_apply(startup, base).is_none());

        let periodic = ObservedClockSyncSample {
            server_instance_id: "server-a".to_string(),
            local_send_us: 10_000,
            local_receive_us: 10_200,
            remote_receive_us: 10_010,
            remote_send_us: 10_015,
            sequence: Some(WEBSOCKET_CLOCK_SYNC_STARTUP_PROBE_COUNT),
        };
        let applied = state
            .select_sample_to_apply(periodic, base + Duration::from_secs(30))
            .expect("first periodic probe should flush the buffered startup sample");

        assert_eq!(applied.sequence, Some(0));
        assert_eq!(applied.total_rtt_us(), 50);
    }

    #[test]
    fn clock_sync_startup_flushes_best_sample_when_window_expires() {
        let mut state = ClockSyncStartupState::default();
        let base = Instant::now();

        for (sequence, local_send_us, local_receive_us) in [(0, 1_000, 1_090), (1, 2_000, 2_040)] {
            let sample = ObservedClockSyncSample {
                server_instance_id: "server-a".to_string(),
                local_send_us,
                local_receive_us,
                remote_receive_us: local_send_us + 10,
                remote_send_us: local_send_us + 15,
                sequence: Some(sequence),
            };
            let observed_at = base + Duration::from_millis(sequence * 100);
            assert!(state.select_sample_to_apply(sample, observed_at).is_none());
        }

        assert!(state
            .take_best_sample_if_startup_window_elapsed(
                base + WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW - Duration::from_millis(1)
            )
            .is_none());

        let applied = state
            .take_best_sample_if_startup_window_elapsed(
                base + WEBSOCKET_CLOCK_SYNC_STARTUP_SELECTION_WINDOW + Duration::from_millis(100),
            )
            .expect("startup timeout should flush the best buffered startup sample");

        assert_eq!(applied.sequence, Some(1));
        assert_eq!(applied.total_rtt_us(), 40);
    }
}

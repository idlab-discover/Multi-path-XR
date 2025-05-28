use clap::Parser;
use rust_socketio::{ClientBuilder, Payload, RawClient};
use serde_json::json;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use sysinfo::{System, get_current_pid, Networks};
use tracing::{info, error, debug};
use regex::Regex;
use tracing_subscriber::FmtSubscriber;
use std::process::{Child, Command, Stdio};
use std::io::{BufRead, BufReader};

#[derive(Parser, Debug)]
#[command(author, version, about = "pc-agent")]
struct Args {
    /// The URL of the controller to connect to (e.g., http://localhost:3000)
    #[clap(short, long, default_value = "http://localhost:3000")]
    url: String,
    /// The node id of the agent (e.g., n1)
    #[clap(short, long, default_value = "n0")]
    node_id: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting agent");

    let args = Args::parse();
    info!("{:?}", args);

    // Check for duplicate processes
    if let Err(e) = kill_duplicate_processes(&args.node_id) {
        error!("Failed to check for duplicate processes: {}", e);
        return Err(e);
    }

    let process = Arc::new(Mutex::new(None));
    let thread_pool = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));

    let url = args.url.clone();
    let node_id = args.node_id.clone();

    let socket_id = Arc::new(RwLock::new(None));
    let socket_id_ref = Arc::clone(&socket_id);

    // Build the Socket.IO client
    let client = match ClientBuilder::new(&url)
        .on("connect", |_, _| {
            info!("Connected to controller");
        })
        .on("disconnect", |_, _| {
            info!("Disconnected from controller");
        })
        .on("close", |_, _| info!("Closed WebSocket connection"))
        .on("error", |err, _| error!("Error: {:#?}", err))
        .on_with_ack("has_connected", {
            let socket_id_ref = Arc::clone(&socket_id_ref);
            let node_id = node_id.clone();
            move |payload: Payload, s: RawClient, ack: i32| {
                let _ = s.ack(ack, "Ok".to_string());

                if let Payload::Text(values) = payload {
                    if let Some(socket_id) = values.first().and_then(|v| v.as_str()) {
                        match socket_id_ref.write() {
                            Ok(mut socket_id_lock) => {
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
                        // Wait one second before sending the logs
                        thread::sleep(Duration::from_secs(1));

                        emit_log(&s, "info", &format!("WebSocket connected with id: {} for {}", socket_id, node_id));
                    }
                }
            }
        })
        .on("update_network_conditions", {
            move |payload, socket| {
                if let Payload::Text(data) = payload {
                    if data.len() != 1 {
                        emit_log(&socket, "error", "Invalid payload format: expected a single object");
                        return;
                    }
                    let serde_json::Value::Object(json_data) = data[0].clone() else {
                        emit_log(&socket, "error", "Failed to parse JSON payload");
                        return;
                    };

                    let bandwidth_mbit = json_data["bandwidth"].as_str().unwrap_or("1000mbit") as &str;
                    let latency_ms = json_data["latency"].as_str().unwrap_or("0ms") as &str;
                    let loss_percent = json_data["loss"].as_str().unwrap_or("0%") as &str;
                    let interface = json_data["interface"].as_str().unwrap_or("") as &str;

                    // Get all interfaces
                    let mut interfaces = get_all_interfaces();

                    // By default, we use all the interfaces except a select few from a blacklist
                    if interfaces.is_empty() {
                        // Remove the 'lo' interface
                        interfaces.retain(|i| i != "lo");
                        // Remove the 'docker0' interface
                        interfaces.retain(|i| i != "docker0");
                        // Remove the 'nat0' interface
                        interfaces.retain(|i| i != "nat0");
                        // Remove the interfaces starting with "enp"
                        interfaces.retain(|i| !i.starts_with("enp"));
                        // Remove the interfaces starting with "wlp"
                        interfaces.retain(|i| !i.starts_with("wlp"));
                    } else {
                        // If the interface is specified, use only that one
                        interfaces.retain(|i| i == interface);
                    }

                    // Attempt to set the network conditions
                    match set_network_conditions(&interfaces, bandwidth_mbit, latency_ms, loss_percent) {
                        Ok(result) => {
                            // Emit the results to the controller
                            let result = result.join(" , \n");
                            emit_log(&socket, "info", result.as_str());
                            emit_log(&socket, "info", &format!(
                                "Successfully applied network conditions on {:?}: {} Mbit, {} ms, {}% loss",
                                interfaces, bandwidth_mbit, latency_ms, loss_percent
                            ));
                        }
                        Err(e) => {
                            emit_log(&socket, "error", &format!(
                                "Failed to set network conditions: {}", e
                            ));
                        }
                    }
                } else {
                    emit_log(&socket, "error", "Invalid payload for update_network_conditions");
                }
            }
        })        
        .on("start_process", {
            let process = Arc::clone(&process);
            let thread_pool = Arc::clone(&thread_pool);
            move |payload, socket| {
                if let Payload::Text(data) = payload {
                    emit_log(&socket.clone(), "info", &format!("Received start_process command: {:?}", data));
                    let mut args: Vec<String> = data.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                    // All the strings should be split by spaces
                    args = args.iter().flat_map(|s| s.split_whitespace().map(String::from)).collect();
                    if !args.is_empty() {
                        let process_clone = Arc::clone(&process);
                        let socket_clone = socket.clone();
                        match thread_pool.lock() {
                            Ok(mut pool) => {
                                pool.push(thread::spawn(move || {
                                    start_process(process_clone, args, socket_clone);
                                }));
                            }
                            Err(e) => {
                                error!("Failed to acquire lock on thread_pool: {}", e);
                            }
                        };
                    } else {
                        emit_log(&socket, "error", "Received empty start_process command");
                    }
                }
            }
        })
        .on("stop_process", {
            let process = Arc::clone(&process);
            let thread_pool = Arc::clone(&thread_pool);
            move |_, socket| {
                let process_clone = Arc::clone(&process);
                let socket_clone = socket.clone();
                match thread_pool.lock() {
                    Ok(mut pool) => {
                        pool.push(thread::spawn(move || {
                            stop_process(process_clone, socket_clone);
                        }));
                    }
                    Err(e) => {
                        error!("Failed to acquire lock on thread_pool: {}", e);
                    }
                }
            }
        })
        .connect()
    {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(err) => {
            error!("Failed to connect WebSocket: {:#?}", err);
            return Ok(());
        }
    };

    info!("Agent connected to controller at {}", url);

    // Keep the main thread alive
    loop {
        clean_threads(&thread_pool);
        debug!("Main loop is running...");
        thread::sleep(Duration::from_secs(30));
        if let Ok(client_lock) = client.lock() {
            let payload = json!({ "level": "info", "data": "I'm still running!" });
            if let Err(e) = client_lock.emit("process_output", payload) {
                error!("Failed to heartbeat: {}", e);
            }
        } else {
            error!("Failed to acquire lock on client");
        }
    }
}

/// Emit a log message to the Socket.IO server
fn emit_log(socket: &RawClient, level: &str, data: &str) {
    // Sanitize the log message to remove unwanted characters
    let sanitized_data = sanitize_log(data);
    /* 
    if level == "error" {
        error!("{}", sanitized_data);
    } else {
        info!("{}", sanitized_data);
    }
    */
    let payload = json!({ "level": level, "data": sanitized_data });
    if let Err(e) = socket.emit("process_output", payload) {
        error!("Failed to emit log: {}", e);
    }
}

/// Sanitize log messages to remove unwanted characters
fn sanitize_log(data: &str) -> String {
    // Regex to match ANSI escape codes
    let ansi_escape = Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap_or_else(|e| {
        error!("Failed to compile regex: {}", e);
        Regex::new("").unwrap()
    });

    // Remove ANSI escape codes and filter out other control characters
    ansi_escape.replace_all(data, "").replace(|c: char| c.is_control(), "").to_string()
}

/// Clean up completed threads from the thread pool
fn clean_threads(thread_pool: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    match thread_pool.lock() {
        Ok(mut pool) => {
            pool.retain(|handle| !handle.is_finished());
        }
        Err(e) => {
            error!("Failed to acquire lock on thread_pool: {}", e);
        }
    }
}

/// Kill duplicate processes with the same node_id
fn kill_duplicate_processes(node_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut system = System::new_all();
    system.refresh_all();
    let current_pid = get_current_pid()?;

    for (pid, process) in system.processes() {
        if *pid != current_pid
            && process.name().to_string_lossy().contains("pc-agent")
        {
            // Combine all arguments into a single string
            let args = process.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
            let parent_pid = process.parent().unwrap_or_else(|| 0.into());
            if parent_pid != current_pid
                && args.contains(&format!("--node-id {}", node_id)) {
                info!("Killing duplicate process: PID {}, Command {:?}", process.pid(), process.cmd());
                info!("The parent PID is: {}", process.parent().unwrap_or_else(|| 0.into()));
                if !process.kill() {
                    error!("Failed to kill process: PID {}", process.pid());

                }
            }
        }
    }

    Ok(())
}

fn start_process(process: Arc<Mutex<Option<Child>>>, command_args: Vec<String>, socket: RawClient) {
    stop_process(process.clone(), socket.clone());

    if command_args.is_empty() {
        emit_log(&socket, "error", "No command provided to start_process");
        return;
    }

    let mut command = Command::new(&command_args[0]);
    if command_args.len() > 1 {
        command.args(&command_args[1..]);
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    info!("Starting process: {:?}", command_args);

    match command.spawn() {
        Ok(mut child) => {
            if let Ok(mut process_guard) = process.lock() {
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let socket_clone_stdout = socket.clone();
                let socket_clone_stderr = socket.clone();

                if let Some(stdout) = stdout {
                    thread::spawn(move || {
                        let reader = BufReader::new(stdout);
                        for line_result in reader.lines() {
                            match line_result {
                                Ok(line) => emit_log(&socket_clone_stdout, "info", &line),
                                Err(e) => error!("Error reading stdout: {}", e),
                            }
                        }
                    });
                }

                if let Some(stderr) = stderr {
                    thread::spawn(move || {
                        let reader = BufReader::new(stderr);
                        for line_result in reader.lines() {
                            match line_result {
                                Ok(line) => emit_log(&socket_clone_stderr, "error", &line),
                                Err(e) => error!("Error reading stderr: {}", e),
                            }
                        }
                    });
                }

                *process_guard = Some(child);
            } else {
                error!("Failed to acquire lock on process");
                // Attempt to kill the child process since we can't store it
                if let Err(e) = child.kill() {
                    error!("Failed to kill process after lock failure: {}", e);
                }
            }
        }
        Err(e) => {
            emit_log(&socket, "error", &format!("Failed to start process: {}", e));
        }
    }
}

fn stop_process(process: Arc<Mutex<Option<Child>>>, socket: RawClient) {
    match process.lock() {
        Ok(mut process_guard) => {
            if let Some(mut child) = process_guard.take() {
                match child.kill() {
                    Ok(_) => emit_log(&socket, "info", "Process killed"),
                    Err(e) => {
                        // If the process has already exited, an error is expected
                        if e.kind() == std::io::ErrorKind::InvalidInput {
                            emit_log(&socket, "info", "Process has already exited");
                        } else {
                            emit_log(&socket, "error", &format!("Failed to kill process: {}", e));
                        }
                    }
                }
            } else {
                emit_log(&socket, "info", "No process to stop");
            }
        }
        Err(e) => {
            error!("Failed to acquire lock on process: {}", e);
            emit_log(&socket, "error", "Failed to stop process due to lock error");
        }
    }
}

/// Get a list of all available network interfaces on the system.
pub fn get_all_interfaces() -> Vec<String> {
    let networks = Networks::new_with_refreshed_list();
    networks.keys().cloned().collect()
}


pub fn set_network_conditions(
    interfaces: &[String],
    bandwidth_mbit: &str,
    latency_ms: &str,
    loss_percent: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut result = Vec::new();
    result.push(format!(
        "Setting network conditions: {} bandwidth, {} latency, {} loss",
        bandwidth_mbit, latency_ms, loss_percent
    ));
    result.push(format!("Interfaces: {:?}", interfaces));
    for interface in interfaces {
        // 1) Check if the qdisc is currently noqueue or htb
        let show_output = Command::new("sudo")
            .args(["tc", "qdisc", "show", "dev", interface])
            .stderr(Stdio::inherit())
            .output()?;
        
        let qdisc_info = String::from_utf8_lossy(&show_output.stdout);

        // 2) If existing qdisc is "noqueue" or "htb", remove it
        if qdisc_info.contains("noqueue") || qdisc_info.contains("htb") {
            let _ = Command::new("sudo")
                .args(["tc", "qdisc", "del", "dev", interface, "root"])
                .output()?;
        }

        // 3) Add the HTB root qdisc
        let output = Command::new("sudo")
            .args([
                "tc", "qdisc", "add", "dev", interface,
                "root", "handle", "1:", "htb", "default", "11",
            ])
            .output()?;

        info!("{:?}", output.stdout);

        // 4) Add the classes for overall rate (1:1) and default flow (1:11)
        let output = Command::new("sudo")
            .args([
                "tc", "class", "add", "dev", interface,
                "parent", "1:", "classid", "1:1",
                "htb", "rate", bandwidth_mbit,
            ])
            .output()?;

        info!("{:?}", output.stdout);

        let output = Command::new("sudo")
            .args([
                "tc", "class", "add", "dev", interface,
                "parent", "1:1", "classid", "1:11",
                "htb", "rate", bandwidth_mbit,
            ])
            .output()?;

        info!("{:?}", output.stdout);

        // 5) Attach netem for latency and loss
        let output = Command::new("sudo")
            .args([
                "tc", "qdisc", "add", "dev", interface,
                "parent", "1:11", "handle", "10:",
                "netem", "limit", "5000", // Set the packet limit to 5000
                "delay", latency_ms,
                "loss", loss_percent,
            ])
            .output()?;

        info!("{:?}", output.stdout);
    }

    Ok(result)
}
use crate::logging::emit_log;
use rust_socketio::RawClient;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;
use sysinfo::{get_current_pid, Signal, System};
use tracing::{error, info, warn};

pub type ManagedProcesses = Arc<Mutex<Vec<Child>>>;

struct ProcessPrefix {
    value: Mutex<String>,
    version: AtomicUsize,
}

impl ProcessPrefix {
    fn new(initial: String) -> Self {
        Self {
            value: Mutex::new(initial),
            version: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> (usize, String) {
        let value = match self.value.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let version = self.version.load(Ordering::Acquire);
        (version, value)
    }

    fn refresh_if_needed(&self, cache: &mut String, cache_version: &mut usize) {
        let current_version = self.version.load(Ordering::Acquire);
        if current_version != *cache_version {
            *cache = match self.value.lock() {
                Ok(value) => value.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            *cache_version = current_version;
        }
    }

    fn update(&self, new_value: String) {
        if let Ok(mut value) = self.value.lock() {
            *value = new_value;
        }
        self.version.fetch_add(1, Ordering::Release);
    }
}

#[derive(Debug)]
struct ReservedPort {
    _listener_v4: TcpListener,
    port: u16,
}

impl ReservedPort {
    fn port(&self) -> u16 {
        self.port
    }
}

pub fn kill_duplicate_processes(node_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    const GRACE_PERIOD: Duration = Duration::from_secs(1);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    let mut system = System::new_all();
    system.refresh_all();
    let current_pid = get_current_pid()?;

    struct ProcessInfo {
        pid: sysinfo::Pid,
        cmd: Vec<String>,
        parent_pid: sysinfo::Pid,
    }

    let candidates: Vec<ProcessInfo> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            if *pid == current_pid {
                return None;
            }
            if !process.name().to_string_lossy().contains("pc-agent") {
                return None;
            }
            let args = process
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let parent_pid = process.parent().unwrap_or_else(|| 0.into());
            if parent_pid == current_pid || !args.contains(&format!("--node-id {node_id}")) {
                return None;
            }
            Some(ProcessInfo {
                pid: *pid,
                cmd: process
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect(),
                parent_pid,
            })
        })
        .collect();

    for candidate in candidates {
        let pid = candidate.pid;

        info!(
            "Gracefully stopping duplicate process: PID {}, Command {:?}",
            pid, candidate.cmd
        );
        info!("The parent PID is: {}", candidate.parent_pid);

        let should_term = system
            .process(pid)
            .map(|p| p.kill_with(Signal::Term).is_some())
            .unwrap_or(false);

        if !should_term {
            error!("Failed to send SIGTERM to process: PID {}", pid);
            continue;
        }

        let deadline = Instant::now() + GRACE_PERIOD;
        let mut exited = false;

        while Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
            system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

            if system.process(pid).is_none() {
                info!("Duplicate process exited gracefully: PID {}", pid);
                exited = true;
                break;
            }
        }

        if exited {
            continue;
        }

        info!(
            "Process did not exit after {:?}, escalating to SIGKILL: PID {}",
            GRACE_PERIOD, pid
        );

        if let Some(still_running) = system.process(pid) {
            if !still_running.kill() {
                error!("Failed to SIGKILL process: PID {}", still_running.pid());
            }
        }
    }

    Ok(())
}

fn reserve_lowest_available_port(
    start_inclusive: u16,
    end_inclusive: u16,
) -> Result<ReservedPort, std::io::Error> {
    if start_inclusive > end_inclusive {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid port range: start ({start_inclusive}) is greater than end ({end_inclusive})"
            ),
        ));
    }

    let mut last_error: Option<std::io::Error> = None;

    for port in start_inclusive..=end_inclusive {
        let addr_v4 = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        match TcpListener::bind(addr_v4) {
            Ok(listener_v4) => {
                return Ok(ReservedPort {
                    _listener_v4: listener_v4,
                    port,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                last_error = Some(e);
            }
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            format!(
                "no available port found in deterministic range {}-{}",
                start_inclusive, end_inclusive
            ),
        )
    }))
}

fn replace_dynamic_port_placeholders(command_args: &[String], port: u16) -> Vec<String> {
    let port_str = port.to_string();
    command_args
        .iter()
        .map(|arg| arg.replace("DYNAMIC_PORT", &port_str))
        .collect()
}

pub fn start_process(processes: ManagedProcesses, command_args: Vec<String>, socket: RawClient) {
    if command_args.is_empty() {
        emit_log(
            &socket,
            "error",
            true,
            "No command provided to start_process",
        );
        return;
    }

    let contains_dynamic_port = command_args.iter().any(|arg| arg.contains("DYNAMIC_PORT"));

    let mut reserved_port = if contains_dynamic_port {
        match reserve_lowest_available_port(5000, 6000) {
            Ok(port) => {
                emit_log(
                    &socket,
                    "info",
                    true,
                    &format!(
                        "Reserved deterministic port {} for command '{}'",
                        port.port(),
                        command_args.join(" ")
                    ),
                );
                Some(port)
            }
            Err(e) => {
                emit_log(
                    &socket,
                    "error",
                    true,
                    &format!("Failed to reserve a deterministic port in range 5000-6000: {e}"),
                );
                return;
            }
        }
    } else {
        None
    };

    let resolved_command_args = match reserved_port.as_ref() {
        Some(port) => replace_dynamic_port_placeholders(&command_args, port.port()),
        None => command_args.clone(),
    };

    let fallback_name = Path::new(&resolved_command_args[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&resolved_command_args[0])
        .to_string();
    let process_prefix = Arc::new(ProcessPrefix::new(format!("[{fallback_name}] ")));
    let fallback_for_resolver = fallback_name;

    let mut command = Command::new(&resolved_command_args[0]);
    if resolved_command_args.len() > 1 {
        command.args(&resolved_command_args[1..]);
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    drop(reserved_port.take());

    match command.spawn() {
        Ok(mut child) => {
            let child_pid = child.id();
            let child_pid_str = format!("#{:05}", child_pid);
            let prefix_for_resolver = Arc::clone(&process_prefix);
            let mut resolver_fallback = Some(fallback_for_resolver);
            if let Some(current_name) = process_name_from_pid(child_pid) {
                if let Some(fallback_value) = resolver_fallback.as_ref() {
                    if &current_name != fallback_value {
                        prefix_for_resolver.update(format!("[{child_pid_str}:{current_name}] "));
                        resolver_fallback = None;
                    }
                }
            }

            if let Some(fallback_value) = resolver_fallback.take() {
                thread::Builder::new()
                    .name("process_name_resolver_thread".to_string())
                    .spawn(move || {
                        if let Some(name) = wait_for_real_process_name(child_pid, &fallback_value) {
                            prefix_for_resolver.update(format!("[{child_pid_str}:{name}] "));
                        }
                    })
                    .expect("Failed to spawn process_name_resolver_thread");
            }

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let socket_clone_stdout = socket.clone();
            let socket_clone_stderr = socket.clone();

            if let Some(stdout) = stdout {
                let prefix = Arc::clone(&process_prefix);
                thread::Builder::new()
                    .name("process_stdout_thread".to_string())
                    .spawn(move || {
                        let reader = BufReader::new(stdout);
                        let (mut prefix_version, mut prefix_str) = prefix.snapshot();
                        for line_result in reader.lines() {
                            match line_result {
                                Ok(line) => {
                                    prefix.refresh_if_needed(&mut prefix_str, &mut prefix_version);
                                    let prefix_len = prefix_str.len();
                                    let mut formatted =
                                        String::with_capacity(prefix_len + line.len());
                                    formatted.push_str(&prefix_str);
                                    formatted.push_str(&line);
                                    emit_log(&socket_clone_stdout, "info", false, &formatted);
                                }
                                Err(e) => error!("Error reading stdout: {}", e),
                            }
                        }
                    })
                    .expect("Failed to spawn process_stdout_thread");
            }

            if let Some(stderr) = stderr {
                let prefix = Arc::clone(&process_prefix);
                thread::Builder::new()
                    .name("process_stderr_thread".to_string())
                    .spawn(move || {
                        let reader = BufReader::new(stderr);
                        let (mut prefix_version, mut prefix_str) = prefix.snapshot();
                        for line_result in reader.lines() {
                            match line_result {
                                Ok(line) => {
                                    prefix.refresh_if_needed(&mut prefix_str, &mut prefix_version);
                                    let prefix_len = prefix_str.len();
                                    let mut formatted =
                                        String::with_capacity(prefix_len + line.len());
                                    formatted.push_str(&prefix_str);
                                    formatted.push_str(&line);
                                    emit_log(&socket_clone_stderr, "error", false, &formatted);
                                }
                                Err(e) => error!("Error reading stderr: {}", e),
                            }
                        }
                    })
                    .expect("Failed to spawn process_stderr_thread");
            }

            match processes.lock() {
                Ok(mut guard) => {
                    guard.push(child);
                    emit_log(
                        &socket,
                        "info",
                        true,
                        &format!(
                            "Started process '{}' (pid: {child_pid}, process count: {})",
                            command_args.join(" "),
                            guard.len()
                        ),
                    );
                }
                Err(e) => {
                    error!("Failed to acquire lock on process: {}", e);
                }
            }
        }
        Err(e) => {
            emit_log(
                &socket,
                "error",
                true,
                &format!("Failed to start process: {e}"),
            );
        }
    }
}

pub fn stop_process(processes: ManagedProcesses, socket: RawClient) {
    match processes.lock() {
        Ok(mut guard) => {
            let mut still_running = 0;
            while let Some(mut child) = guard.pop() {
                match child.kill() {
                    Ok(_) => emit_log(&socket, "info", true, "Process killed"),
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::InvalidInput {
                            emit_log(&socket, "info", true, "Process has already exited");
                        } else {
                            emit_log(
                                &socket,
                                "error",
                                true,
                                &format!("Failed to kill process: {e}"),
                            );
                        }
                    }
                }
                let _ = child.wait();
                still_running += 1;
            }
            emit_log(
                &socket,
                "info",
                true,
                &format!("Stopped {still_running} managed processes"),
            );
        }
        Err(e) => {
            error!("Failed to acquire lock on process: {}", e);
            emit_log(
                &socket,
                "error",
                true,
                "Failed to stop processes due to lock error",
            );
        }
    }
}

pub fn reap_exited_processes(processes: &ManagedProcesses) {
    let Ok(mut guard) = processes.lock() else {
        return;
    };

    guard.retain_mut(|child| match child.try_wait() {
        Ok(Some(_status)) => {
            info!("Reaped exited process with PID {}", child.id());
            false
        }
        Ok(None) => true,
        Err(_) => true,
    });
}

pub fn shutdown_managed_processes(processes: &ManagedProcesses) {
    if let Ok(mut guard) = processes.lock() {
        while let Some(mut child) = guard.pop() {
            match child.kill() {
                Ok(_) => info!("Child process killed on shutdown"),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::InvalidInput {
                        warn!("Process has already exited");
                    } else {
                        error!("Failed to kill child process on shutdown: {e}");
                    }
                }
            };
            let _ = child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
fn process_name_from_pid(pid: u32) -> Option<String> {
    let path = format!("/proc/{pid}/comm");
    if let Ok(mut name) = std::fs::read_to_string(path) {
        while name.ends_with('\n') || name.ends_with('\r') {
            name.pop();
        }
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn process_name_from_pid(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn wait_for_real_process_name(pid: u32, fallback: &str) -> Option<String> {
    let start = Instant::now();
    let timeout = Duration::from_secs(5);
    let poll_interval = Duration::from_millis(20);
    while start.elapsed() < timeout {
        match process_name_from_pid(pid) {
            Some(name) => {
                if name != fallback {
                    return Some(name);
                }
            }
            None => return None,
        }
        thread::sleep(poll_interval);
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn wait_for_real_process_name(_pid: u32, _fallback: &str) -> Option<String> {
    None
}

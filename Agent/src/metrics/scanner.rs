use crate::metrics::config::MetricsScannerConfig;
use crate::metrics::parser::{inject_agent_labels, parse_prometheus_text};
use crate::metrics::state::{PortHealth, PortScanState};
use crate::metrics::store::{MetricsStore, TargetMetricsSnapshot};

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use tracing::{debug, warn};

const LOCALHOST_IP: &str = "127.0.0.1";

/// Starts the background scanner thread.
pub fn start_metrics_scanner(
    config: MetricsScannerConfig,
    store: Arc<MetricsStore>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) -> Result<JoinHandle<()>, std::io::Error> {
    thread::Builder::new()
        .name("metrics_scanner_thread".to_string())
        .spawn(move || run_metrics_scanner(config, store, shutdown, node_id))
}

fn run_metrics_scanner(
    config: MetricsScannerConfig,
    store: Arc<MetricsStore>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) {
    let mut states = BTreeMap::new();
    let mut last_discovery_at: Option<Instant> = None;

    while !shutdown.load(Ordering::Acquire) {
        let round_started = Instant::now();
        let wall_clock_now = SystemTime::now();

        let discovery_due = last_discovery_at.map_or(true, |last_discovery_at| {
            round_started.duration_since(last_discovery_at) >= config.discovery_interval
        });

        if discovery_due {
            let candidate_ports = discover_candidate_ports(&config).unwrap_or_else(|error| {
                warn!(
                    "Failed to discover listening ports with ss; falling back to configured range {}-{}: {}",
                    config.port_start,
                    config.port_end,
                    error
                );
                full_range_candidate_ports(&config)
            });
            sync_port_states(&mut states, &candidate_ports, round_started);
            last_discovery_at = Some(round_started);
        }

        let mut active_ports = Vec::new();
        let mut discovery_ports = Vec::new();

        for state in states.values() {
            if !state.due(round_started) {
                continue;
            }

            match state.health {
                PortHealth::Active => active_ports.push(state.port),
                PortHealth::Unknown | PortHealth::Backoff if discovery_due => {
                    discovery_ports.push(state.port)
                }
                PortHealth::Unknown | PortHealth::Backoff => {}
            }
        }

        let scrape_results = execute_bounded_jobs(
            &active_ports,
            config.scrape_concurrency,
            Arc::clone(&shutdown),
            {
                let config = config.clone();
                move |port| scrape_metrics(port, &config, config.scrape_connect_timeout)
            },
        );

        let discovery_results = execute_bounded_jobs(
            &discovery_ports,
            config.discovery_concurrency,
            Arc::clone(&shutdown),
            {
                let config = config.clone();
                move |port| scrape_metrics(port, &config, config.discovery_connect_timeout)
            },
        );

        let mut observed_ports = BTreeSet::new();

        for (port, result) in scrape_results
            .into_iter()
            .chain(discovery_results.into_iter())
        {
            let Some(state) = states.get_mut(&port) else {
                continue;
            };

            match result {
                Ok(body) => {
                    observed_ports.insert(port);

                    let scrape_started = Instant::now();
                    let mut report = parse_prometheus_text(&body);
                    for sample in &mut report.samples {
                        *sample = inject_agent_labels(sample, &node_id, LOCALHOST_IP, port);
                    }

                    let raw_body = if config.keep_raw_body {
                        body.clone()
                    } else {
                        String::new()
                    };

                    debug!(
                        "Discovered metrics exporter on {}:{} with {} samples",
                        LOCALHOST_IP,
                        port,
                        report.samples.len()
                    );

                    store.upsert_target(TargetMetricsSnapshot {
                        port,
                        source_ip: LOCALHOST_IP.to_string(),
                        agent_node_id: node_id.clone(),
                        source_instance: format!("{node_id}@{LOCALHOST_IP}:{port}"),
                        scraped_at: wall_clock_now,
                        scrape_duration: scrape_started.elapsed(),
                        scrape_ok: true,
                        error: None,
                        raw_body,
                        samples: report.samples,
                        malformed_lines: report.malformed_lines,
                    });

                    state.mark_success(round_started, wall_clock_now, config.scan_interval);
                }
                Err(error) => {
                    let had_successful_snapshot = state.last_success_at.is_some();
                    state.mark_failure(round_started, error.clone(), config.max_probe_backoff);

                    if had_successful_snapshot {
                        observed_ports.insert(port);
                        store.upsert_target(TargetMetricsSnapshot {
                            port,
                            source_ip: LOCALHOST_IP.to_string(),
                            agent_node_id: node_id.clone(),
                            source_instance: format!("{node_id}@{LOCALHOST_IP}:{port}"),
                            scraped_at: wall_clock_now,
                            scrape_duration: Duration::ZERO,
                            scrape_ok: false,
                            error: Some(error),
                            raw_body: String::new(),
                            samples: Vec::new(),
                            malformed_lines: 0,
                        });
                    }
                }
            }
        }

        store.retain_only_ports(&observed_ports);
        store.complete_scan_round(SystemTime::now(), round_started.elapsed());

        debug!(
            "Metrics scanner round completed; discovery_triggered={}, known_due={}, discovery_probe_due={}, tracked_targets={}, duration_ms={}",
            discovery_due,
            active_ports.len(),
            discovery_ports.len(),
            store.target_count(),
            round_started.elapsed().as_millis()
        );

        sleep_until_next_tick(round_started, config.scan_interval, &shutdown);
    }
}

fn full_range_candidate_ports(config: &MetricsScannerConfig) -> BTreeSet<u16> {
    let mut ports = BTreeSet::new();
    for port in config.port_start..=config.port_end {
        if config.is_excluded(port) {
            continue;
        }
        ports.insert(port);
    }
    ports
}

fn sync_port_states(
    states: &mut BTreeMap<u16, PortScanState>,
    candidate_ports: &BTreeSet<u16>,
    now: Instant,
) {
    states.retain(|port, _| candidate_ports.contains(port));

    for port in candidate_ports {
        states
            .entry(*port)
            .or_insert_with(|| PortScanState::new(*port, now));
    }
}

fn discover_candidate_ports(config: &MetricsScannerConfig) -> Result<BTreeSet<u16>, String> {
    if !config.prefer_ss_listening_port_discovery {
        return Ok(full_range_candidate_ports(config));
    }

    let output = Command::new(&config.ss_command)
        .args(["-ltnH"])
        .output()
        .map_err(|error| format!("failed to execute '{}': {}", config.ss_command, error))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "'{} -ltnH' exited with status {}: {}",
            config.ss_command,
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("ss output was not valid UTF-8: {}", error))?;

    let mut ports = BTreeSet::new();
    for line in stdout.lines() {
        if let Some(port) = parse_listening_port_from_ss_line(line) {
            if port < config.port_start || port > config.port_end || config.is_excluded(port) {
                continue;
            }
            ports.insert(port);
        }
    }

    Ok(ports)
}

fn parse_listening_port_from_ss_line(line: &str) -> Option<u16> {
    let local_address = line.split_whitespace().nth(3)?;
    parse_port_from_socket_address(local_address)
}

fn parse_port_from_socket_address(value: &str) -> Option<u16> {
    if let Some(rest) = value.strip_prefix('[') {
        let (_, port) = rest.rsplit_once("]:")?;
        return port.parse::<u16>().ok();
    }

    let (_, port) = value.rsplit_once(':')?;
    port.parse::<u16>().ok()
}

fn execute_bounded_jobs<F>(
    ports: &[u16],
    concurrency: usize,
    shutdown: Arc<AtomicBool>,
    job: F,
) -> Vec<(u16, Result<String, String>)>
where
    F: Fn(u16) -> Result<String, String> + Send + Sync + 'static,
{
    if ports.is_empty() {
        return Vec::new();
    }

    let worker_count = concurrency.min(ports.len()).max(1);
    let job = Arc::new(job);
    let (tx, rx) = mpsc::channel();

    let chunks = split_ports(ports, worker_count);
    let mut handles = Vec::with_capacity(chunks.len());

    for chunk in chunks {
        let tx = tx.clone();
        let job = Arc::clone(&job);
        let shutdown = Arc::clone(&shutdown);

        handles.push(thread::spawn(move || {
            for port in chunk {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }

                let result = job(port);
                if tx.send((port, result)).is_err() {
                    break;
                }
            }
        }));
    }

    drop(tx);

    let mut out = Vec::with_capacity(ports.len());
    for item in rx {
        out.push(item);
    }

    for handle in handles {
        let _ = handle.join();
    }

    out
}

fn split_ports(ports: &[u16], workers: usize) -> Vec<Vec<u16>> {
    let mut out = vec![Vec::new(); workers];
    for (idx, port) in ports.iter().copied().enumerate() {
        out[idx % workers].push(port);
    }
    out
}

fn scrape_metrics(
    port: u16,
    config: &MetricsScannerConfig,
    connect_timeout: Duration,
) -> Result<String, String> {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let mut stream =
        TcpStream::connect_timeout(&addr, connect_timeout).map_err(|e| e.to_string())?;

    stream
        .set_read_timeout(Some(config.read_timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(config.write_timeout))
        .map_err(|e| e.to_string())?;

    // We probe every discovered TCP listener with a plain HTTP scrape request.
    // TLS-only listeners in the configured port range will reject this and may log
    // a harmless handshake warning on the server side while the scanner keeps looking
    // for real Prometheus exporters.
    let request = format!(
        "GET /metrics HTTP/1.1\r\nHost: {LOCALHOST_IP}:{port}\r\nConnection: close\r\nAccept: text/plain\r\n\r\n"
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;

    let mut response = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 16 * 1024];

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&chunk[..n]);
                if response.len() > config.max_response_bytes {
                    return Err(format!(
                        "response exceeded maximum allowed size of {} bytes",
                        config.max_response_bytes
                    ));
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }

    let text = String::from_utf8(response).map_err(|e| e.to_string())?;
    parse_http_metrics_response(&text)
}

fn parse_http_metrics_response(response: &str) -> Result<String, String> {
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "invalid HTTP response: missing header terminator".to_string())?;

    let mut lines = headers.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "invalid HTTP response: missing status line".to_string())?;

    let mut status_parts = status_line.split_ascii_whitespace();
    let _http_version = status_parts
        .next()
        .ok_or_else(|| "invalid HTTP response: missing HTTP version".to_string())?;
    let status_code = status_parts
        .next()
        .ok_or_else(|| "invalid HTTP response: missing status code".to_string())?
        .parse::<u16>()
        .map_err(|_| "invalid HTTP response: malformed status code".to_string())?;

    if status_code != 200 {
        return Err(format!("unexpected HTTP status code {status_code}"));
    }

    Ok(body.to_string())
}

fn sleep_until_next_tick(started_at: Instant, interval: Duration, shutdown: &AtomicBool) {
    let elapsed = started_at.elapsed();
    let remaining = interval.saturating_sub(elapsed);
    sleep_interruptibly(remaining, shutdown);
}

fn sleep_interruptibly(duration: Duration, shutdown: &AtomicBool) {
    const STEP: Duration = Duration::from_millis(20);

    let started = Instant::now();
    while started.elapsed() < duration {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        let remaining = duration.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(STEP));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_response_body() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nup 1\n";
        let parsed = parse_http_metrics_response(response).unwrap();
        assert_eq!(parsed, "up 1\n");
    }

    #[test]
    fn rejects_non_200_status() {
        let response = "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\n\r\nnope\n";
        let err = parse_http_metrics_response(response).unwrap_err();
        assert!(err.contains("404"));
    }

    #[test]
    fn parses_port_from_ss_ipv4_line() {
        let line = "LISTEN 0 4096 127.0.0.1:9090 0.0.0.0:*";
        assert_eq!(parse_listening_port_from_ss_line(line), Some(9090));
    }

    #[test]
    fn parses_port_from_ss_ipv6_line() {
        let line = "LISTEN 0 4096 [::1]:9100 [::]:*";
        assert_eq!(parse_listening_port_from_ss_line(line), Some(9100));
    }

    #[test]
    fn splits_ports_across_workers() {
        let ports = vec![3001, 3002, 3003, 3004, 3005];
        let chunks = split_ports(&ports, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], vec![3001, 3003, 3005]);
        assert_eq!(chunks[1], vec![3002, 3004]);
    }
}

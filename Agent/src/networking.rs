#[cfg(target_os = "linux")]
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs;
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use sysinfo::Networks;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct RouteNexthop {
    pub via: String,
    pub dev: String,
    pub weight: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteUpdateRequest {
    pub route: String,
    pub nexthops: Vec<RouteNexthop>,
}

#[derive(Debug, Clone)]
pub struct RouteUpdateResult {
    pub applied: bool,
    pub route: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct LastRouteState {
    last_applied: Instant,
    nexthops: Vec<RouteNexthop>,
}

static ROUTE_UPDATE_STATE: OnceLock<Mutex<HashMap<String, LastRouteState>>> = OnceLock::new();

fn route_update_state() -> &'static Mutex<HashMap<String, LastRouteState>> {
    ROUTE_UPDATE_STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get_all_interfaces() -> Vec<String> {
    let networks = Networks::new_with_refreshed_list();
    let mut interfaces: Vec<String> = networks.keys().cloned().collect();
    interfaces.sort();
    interfaces
}

fn default_network_condition_interfaces() -> Vec<String> {
    let mut interfaces = get_all_interfaces();
    interfaces.retain(|i| i != "lo");
    interfaces.retain(|i| i != "docker0");
    interfaces.retain(|i| i != "nat0");
    interfaces.retain(|i| !i.starts_with("enp"));
    interfaces.retain(|i| !i.starts_with("wlp"));
    interfaces
}

#[cfg(target_os = "linux")]
fn interfaces_for_ip(target_ip: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let normalized_ip = target_ip.trim().split('/').next().unwrap_or("").trim();
    if normalized_ip.is_empty() {
        return Ok(Vec::new());
    }

    let output = Command::new("ip").args(["-o", "-4", "addr", "show"]).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit status {}", output.status)
        };
        return Err(format!("failed to resolve interface for IP {normalized_ip}: {detail}").into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut interfaces = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let _index = parts.next();
        let Some(raw_interface) = parts.next() else {
            continue;
        };
        let Some(family) = parts.next() else {
            continue;
        };
        if family != "inet" {
            continue;
        }
        let Some(address_with_prefix) = parts.next() else {
            continue;
        };
        let address = address_with_prefix.split('/').next().unwrap_or("").trim();
        if address != normalized_ip {
            continue;
        }

        let interface_name = raw_interface
            .split('@')
            .next()
            .unwrap_or(raw_interface)
            .trim()
            .to_string();
        if !interface_name.is_empty() && !interfaces.contains(&interface_name) {
            interfaces.push(interface_name);
        }
    }

    interfaces.sort();
    Ok(interfaces)
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtualWallManifestInterface {
    client_id: String,
    component_device: Option<String>,
    mac: Option<String>,
    ipv4_address: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtualWallManifestNode {
    client_id: String,
    host: Option<String>,
    login_hostname: Option<String>,
    vnode_name: Option<String>,
    component_node_name: Option<String>,
}

#[cfg(target_os = "linux")]
static VIRTUALWALL_INTERFACE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();

#[cfg(target_os = "linux")]
static VIRTUALWALL_NODE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();

#[cfg(target_os = "linux")]
fn virtualwall_interface_block_re() -> &'static Regex {
    VIRTUALWALL_INTERFACE_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?s)<interface(?:\s|>)[^>]*>.*?</interface>")
            .expect("valid Virtual Wall interface regex")
    })
}

#[cfg(target_os = "linux")]
fn virtualwall_node_block_re() -> &'static Regex {
    VIRTUALWALL_NODE_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?s)<node(?:\s|>)[^>]*>.*?</node>")
            .expect("valid Virtual Wall node regex")
    })
}

#[cfg(target_os = "linux")]
fn xml_attr(fragment: &str, key: &str) -> Option<String> {
    let needle = format!(r#"{key}=""#);
    let start = fragment.find(&needle)? + needle.len();
    let rest = &fragment[start..];
    let end = rest.find('"')?;
    let value = rest[..end].trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

#[cfg(target_os = "linux")]
fn xml_tag_attr(fragment: &str, tag: &str, key: &str) -> Option<String> {
    let needle = format!("<{tag}");
    let start = fragment.find(&needle)?;
    let rest = &fragment[start..];
    let end = rest.find('>')?;
    xml_attr(&rest[..end], key)
}

#[cfg(target_os = "linux")]
fn canonicalize_mac(raw_mac: &str) -> Option<String> {
    let normalized = raw_mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect::<String>();
    if normalized.len() == 12 {
        Some(normalized)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn component_device_name(component_id: &str) -> Option<String> {
    let device = component_id.trim().rsplit(':').next()?.trim();
    if device.is_empty() {
        return None;
    }
    Some(device.to_string())
}

#[cfg(target_os = "linux")]
fn component_node_name(component_id: &str) -> Option<String> {
    let node_name = component_id.split("+node+").nth(1)?.trim();
    if node_name.is_empty() {
        return None;
    }
    Some(node_name.to_string())
}

#[cfg(target_os = "linux")]
fn parse_virtualwall_manifest_interfaces(manifest_contents: &str) -> Vec<VirtualWallManifestInterface> {
    let mut interfaces = Vec::new();
    for block in virtualwall_interface_block_re().find_iter(manifest_contents) {
        let fragment = block.as_str();
        let Some(open_tag_end) = fragment.find('>') else {
            continue;
        };
        let open_tag = &fragment[..=open_tag_end];
        let Some(client_id) = xml_attr(open_tag, "client_id") else {
            continue;
        };

        interfaces.push(VirtualWallManifestInterface {
            client_id,
            component_device: xml_attr(open_tag, "component_id")
                .and_then(|component_id| component_device_name(&component_id)),
            mac: xml_attr(open_tag, "mac_address").and_then(|raw_mac| canonicalize_mac(&raw_mac)),
            ipv4_address: xml_tag_attr(fragment, "ip", "address"),
        });
    }
    interfaces
}

#[cfg(target_os = "linux")]
fn parse_virtualwall_manifest_nodes(manifest_contents: &str) -> Vec<VirtualWallManifestNode> {
    let mut nodes = Vec::new();
    for block in virtualwall_node_block_re().find_iter(manifest_contents) {
        let fragment = block.as_str();
        let Some(open_tag_end) = fragment.find('>') else {
            continue;
        };
        let open_tag = &fragment[..=open_tag_end];
        let Some(client_id) = xml_attr(open_tag, "client_id") else {
            continue;
        };

        nodes.push(VirtualWallManifestNode {
            client_id,
            host: xml_tag_attr(fragment, "host", "name"),
            login_hostname: xml_tag_attr(fragment, "login", "hostname"),
            vnode_name: xml_tag_attr(fragment, "emulab:vnode", "name"),
            component_node_name: xml_attr(open_tag, "component_id")
                .and_then(|component_id| component_node_name(&component_id)),
        });
    }
    nodes
}

#[cfg(target_os = "linux")]
fn identity_matches(field: &str, candidate: &str) -> bool {
    let normalized_field = field.trim().to_ascii_lowercase();
    let normalized_candidate = candidate.trim().to_ascii_lowercase();
    if normalized_field.is_empty() || normalized_candidate.is_empty() {
        return false;
    }

    normalized_field == normalized_candidate
        || normalized_field
            .strip_prefix(&normalized_candidate)
            .is_some_and(|suffix| suffix.starts_with('.'))
        || normalized_candidate
            .strip_prefix(&normalized_field)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

#[cfg(target_os = "linux")]
fn push_unique_string(values: &mut Vec<String>, value: String) {
    if value.is_empty() || values.contains(&value) {
        return;
    }
    values.push(value);
}

#[cfg(target_os = "linux")]
fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return None;
    }
    Some(stdout)
}

#[cfg(target_os = "linux")]
fn collect_virtualwall_identity_candidates() -> Vec<String> {
    let mut candidates = Vec::new();

    for key in ["VW_CLIENT_ID", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                push_unique_string(&mut candidates, trimmed.to_string());
            }
        }
    }

    if let Some(client_id) = command_stdout("geni-get", &["client_id"]) {
        push_unique_string(&mut candidates, client_id);
    }

    if let Ok(nickname) = fs::read_to_string("/var/emulab/boot/nickname") {
        let trimmed = nickname.trim();
        if !trimmed.is_empty() {
            push_unique_string(&mut candidates, trimmed.to_string());
        }
    }

    for args in [["-s"].as_slice(), ["-f"].as_slice(), [].as_slice()] {
        if let Some(hostname) = command_stdout("hostname", args) {
            push_unique_string(&mut candidates, hostname);
        }
    }

    candidates
}

#[cfg(target_os = "linux")]
fn resolve_current_virtualwall_client_id(
    manifest_contents: &str,
    candidates: &[String],
) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }

    parse_virtualwall_manifest_nodes(manifest_contents)
        .into_iter()
        .find(|node| {
            let fields = [
                Some(node.client_id.as_str()),
                node.host.as_deref(),
                node.login_hostname.as_deref(),
                node.vnode_name.as_deref(),
                node.component_node_name.as_deref(),
            ];
            candidates.iter().any(|candidate| {
                fields
                    .iter()
                    .flatten()
                    .any(|field| identity_matches(field, candidate))
            })
        })
        .map(|node| node.client_id)
}

#[cfg(target_os = "linux")]
fn find_virtualwall_manifest_interface(
    manifest_contents: &str,
    requested_interface: &str,
    current_client_id: Option<&str>,
) -> Option<VirtualWallManifestInterface> {
    let normalized_interface = requested_interface.trim();
    if normalized_interface.is_empty() {
        return None;
    }

    let current_prefix = current_client_id
        .map(str::trim)
        .filter(|client_id| !client_id.is_empty())
        .map(|client_id| format!("{client_id}:"));

    parse_virtualwall_manifest_interfaces(manifest_contents)
        .into_iter()
        .find(|interface| {
            if normalized_interface.contains(':') {
                interface.client_id == normalized_interface
            } else if let Some(prefix) = current_prefix.as_deref() {
                interface.client_id.starts_with(prefix)
                    && interface.component_device.as_deref() == Some(normalized_interface)
            } else {
                false
            }
        })
}

#[cfg(target_os = "linux")]
fn resolve_manifest_interface_names(
    manifest_interface: &VirtualWallManifestInterface,
    mac_to_interface: &HashMap<String, String>,
    ip_to_interfaces: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut resolved = Vec::new();

    if let Some(mac) = manifest_interface.mac.as_ref() {
        if let Some(interface_name) = mac_to_interface.get(mac) {
            push_unique_string(&mut resolved, interface_name.clone());
        }
    }

    if resolved.is_empty() {
        if let Some(ipv4_address) = manifest_interface.ipv4_address.as_ref() {
            if let Some(interface_names) = ip_to_interfaces.get(ipv4_address) {
                for interface_name in interface_names {
                    push_unique_string(&mut resolved, interface_name.clone());
                }
            }
        }
    }

    resolved.sort();
    resolved
}

#[cfg(target_os = "linux")]
fn build_local_mac_to_interface_map() -> HashMap<String, String> {
    let mut mac_to_interface = HashMap::new();

    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return mac_to_interface;
    };

    for entry in entries.flatten() {
        let interface_name = entry.file_name().to_string_lossy().trim().to_string();
        if interface_name.is_empty() || interface_name == "lo" {
            continue;
        }

        let address_path = entry.path().join("address");
        let Ok(address) = fs::read_to_string(address_path) else {
            continue;
        };
        let Some(mac) = canonicalize_mac(address.trim()) else {
            continue;
        };
        mac_to_interface.insert(mac, interface_name);
    }

    mac_to_interface
}

#[cfg(target_os = "linux")]
fn virtualwall_manifest_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let mut push_path = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    for key in ["VW_RSPEC_FILE", "MULTIPATHXR_RSPEC_FILE"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                push_path(PathBuf::from(trimmed));
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        push_path(cwd.join("scripts/multipathxr-meta.private.rspec"));
        push_path(cwd.join("multipathxr-meta.private.rspec"));
        if let Some(parent) = cwd.parent() {
            push_path(parent.join("scripts/multipathxr-meta.private.rspec"));
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        for ancestor in current_exe.ancestors().take(4) {
            push_path(ancestor.join("scripts/multipathxr-meta.private.rspec"));
        }
    }

    paths
}

#[cfg(target_os = "linux")]
fn load_virtualwall_manifest_contents() -> Option<String> {
    for path in virtualwall_manifest_candidate_paths() {
        if !path.is_file() {
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(contents) if contents.contains("<rspec") => return Some(contents),
            Ok(_) => continue,
            Err(err) => warn!(
                "Failed to read Virtual Wall manifest candidate '{}': {err}",
                path.display()
            ),
        }
    }

    if let Some(contents) = command_stdout("geni-get", &["manifest"]) {
        if contents.contains("<rspec") {
            return Some(contents);
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn interfaces_for_virtualwall_hint(
    requested_interface: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let normalized_interface = requested_interface.trim();
    if normalized_interface.is_empty() {
        return Ok(Vec::new());
    }

    let Some(manifest_contents) = load_virtualwall_manifest_contents() else {
        return Ok(Vec::new());
    };

    let current_client_id = if normalized_interface.contains(':') {
        None
    } else {
        resolve_current_virtualwall_client_id(
            &manifest_contents,
            &collect_virtualwall_identity_candidates(),
        )
    };

    let Some(manifest_interface) = find_virtualwall_manifest_interface(
        &manifest_contents,
        normalized_interface,
        current_client_id.as_deref(),
    ) else {
        return Ok(Vec::new());
    };

    let mac_to_interface = build_local_mac_to_interface_map();
    let mut ip_to_interfaces = HashMap::new();
    if let Some(ipv4_address) = manifest_interface.ipv4_address.as_deref() {
        match interfaces_for_ip(ipv4_address) {
            Ok(interface_names) if !interface_names.is_empty() => {
                ip_to_interfaces.insert(ipv4_address.to_string(), interface_names);
            }
            Ok(_) => {}
            Err(err) => warn!(
                "Failed to resolve Virtual Wall manifest IP '{}' for hint '{}': {err}",
                ipv4_address,
                normalized_interface,
            ),
        }
    }

    Ok(resolve_manifest_interface_names(
        &manifest_interface,
        &mac_to_interface,
        &ip_to_interfaces,
    ))
}

#[cfg(not(target_os = "linux"))]
fn interfaces_for_ip(_target_ip: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "linux"))]
fn interfaces_for_virtualwall_hint(
    _requested_interface: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(Vec::new())
}

pub fn resolve_network_condition_interfaces(
    requested_interface: Option<&str>,
    requested_interface_ip: Option<&str>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let interface_name = requested_interface.unwrap_or("").trim();
    let interface_ip = requested_interface_ip.unwrap_or("").trim();

    if interface_name.is_empty() && interface_ip.is_empty() {
        return Ok(default_network_condition_interfaces());
    }

    let exact_matches: Vec<String> = get_all_interfaces()
        .into_iter()
        .filter(|name| name == interface_name)
        .collect();
    if !exact_matches.is_empty() {
        info!(
            "Resolved network-condition interface hint '{}' by exact local device name",
            interface_name
        );
        return Ok(exact_matches);
    }

    if !interface_ip.is_empty() {
        let ip_matches = interfaces_for_ip(interface_ip)?;
        if !ip_matches.is_empty() {
            info!(
                "Resolved network-condition interface hint '{}' via local IPv4 '{}': {:?}",
                interface_name,
                interface_ip,
                ip_matches,
            );
            return Ok(ip_matches);
        }
    }

    if !interface_name.is_empty() {
        let virtualwall_matches = interfaces_for_virtualwall_hint(interface_name)?;
        if !virtualwall_matches.is_empty() {
            info!(
                "Resolved network-condition interface hint '{}' via Virtual Wall manifest fallback: {:?}",
                interface_name,
                virtualwall_matches,
            );
            return Ok(virtualwall_matches);
        }
    }

    Ok(Vec::new())
}

fn env_is_truthy(name: &str, default_value: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default_value)
}

fn env_u64(name: &str, default_value: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default_value)
}

fn env_f64(name: &str, default_value: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default_value)
}

#[cfg(target_os = "linux")]
fn expected_missing_qdisc_detail(detail: &str) -> bool {
    let normalized = detail.trim().to_ascii_lowercase();
    normalized.contains("no such file or directory")
        || normalized.contains("cannot delete qdisc with handle of zero")
        || normalized.contains("invalid handle")
}

#[cfg(target_os = "linux")]
fn delete_qdisc_best_effort(
    interface: &str,
    qdisc_kind: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let args = ["tc", "qdisc", "del", "dev", interface, qdisc_kind];
    let output = Command::new("sudo").args(args).output()?;

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    if output.status.success() {
        info!(
            "Cleared startup tc {} qdisc on interface '{}'",
            qdisc_kind, interface
        );
        return Ok(true);
    }

    if expected_missing_qdisc_detail(&detail) {
        return Ok(false);
    }

    warn!(
        "Failed to clear startup tc {} qdisc on interface '{}': {}",
        qdisc_kind, interface, detail
    );
    Ok(false)
}

#[cfg(target_os = "linux")]
pub fn reset_tc_state_on_startup() -> Result<(), Box<dyn std::error::Error>> {
    if !env_is_truthy("PC_AGENT_RESET_TC_ON_STARTUP", false) {
        return Ok(());
    }

    let mut interfaces = get_all_interfaces();
    interfaces.retain(|interface| interface != "lo");
    interfaces.sort();

    if interfaces.is_empty() {
        info!(
            "PC_AGENT_RESET_TC_ON_STARTUP is enabled, but no non-loopback interfaces were found"
        );
        return Ok(());
    }

    info!(
        "Resetting startup tc state on interfaces: {:?}",
        interfaces
    );

    for interface in interfaces {
        for qdisc_kind in ["root", "ingress", "clsact"] {
            let _ = delete_qdisc_best_effort(&interface, qdisc_kind)?;
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn reset_tc_state_on_startup() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_sudo_command(args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let output = Command::new("sudo").args(args).output()?;
    if output.status.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    Err(format!("command failed: sudo {}: {detail}", args.join(" ")).into())
}

#[cfg(target_os = "linux")]
fn log_command_output(command_name: &str, args: &[&str], output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let status = &output.status;

    if status.success() {
        info!(
            "{} executed successfully (sudo {}): exit status {}",
            command_name,
            args.join(" "),
            status
        );
    } else {
        warn!(
            "{} failed (sudo {}): exit status {}",
            command_name,
            args.join(" "),
            status
        );
    }

    if !stdout.is_empty() {
        info!(
            "{} stdout (sudo {}): {}",
            command_name,
            args.join(" "),
            stdout
        );
    }
    if !stderr.is_empty() {
        warn!(
            "{} stderr (sudo {}): {}",
            command_name,
            args.join(" "),
            stderr
        );
    }
}

fn parse_ipv4_cidr(route: &str) -> Option<(Ipv4Addr, u8)> {
    let mut parts = route.trim().split('/');
    let ip_part = parts.next()?;
    let prefix_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let ip = Ipv4Addr::from_str(ip_part).ok()?;
    let prefix = prefix_part.parse::<u8>().ok()?;
    if prefix > 32 {
        return None;
    }

    Some((ip, prefix))
}

fn validate_nexthop(nh: &RouteNexthop) -> Option<String> {
    let via = nh.via.trim();
    let dev = nh.dev.trim();
    if Ipv4Addr::from_str(via).is_err() {
        return Some(format!("invalid nexthop via '{}': expected IPv4", nh.via));
    }

    if dev.is_empty() || dev.chars().any(char::is_whitespace) {
        return Some(format!(
            "invalid interface '{}': cannot be empty or contain whitespace",
            nh.dev
        ));
    }

    if nh.weight == 0 || nh.weight > 256 {
        return Some(format!("invalid weight '{}': expected 1..=256", nh.weight));
    }

    None
}

fn normalize_nexthops(mut nexthops: Vec<RouteNexthop>) -> Vec<RouteNexthop> {
    nexthops.sort_by(|a, b| {
        a.via
            .cmp(&b.via)
            .then_with(|| a.dev.cmp(&b.dev))
            .then_with(|| a.weight.cmp(&b.weight))
    });
    nexthops
}

fn same_nexthop_set(a: &[RouteNexthop], b: &[RouteNexthop]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.via == y.via && x.dev == y.dev)
}

fn max_relative_weight_delta_percent(old_hops: &[RouteNexthop], new_hops: &[RouteNexthop]) -> f64 {
    let old_map: HashMap<(&str, &str), u32> = old_hops
        .iter()
        .map(|nh| ((nh.via.as_str(), nh.dev.as_str()), nh.weight))
        .collect();

    let mut max_delta = 0.0;
    for nh in new_hops {
        let key = (nh.via.as_str(), nh.dev.as_str());
        let old_weight = old_map.get(&key).copied().unwrap_or(0);
        let denominator = old_weight.max(1) as f64;
        let delta = ((nh.weight as f64 - old_weight as f64).abs() / denominator) * 100.0;
        if delta > max_delta {
            max_delta = delta;
        }
    }

    max_delta
}

#[cfg(target_os = "linux")]
pub fn apply_route_update(
    request: &RouteUpdateRequest,
) -> Result<RouteUpdateResult, Box<dyn std::error::Error>> {
    if !env_is_truthy("PC_AGENT_ENABLE_ROUTE_UPDATES", false) {
        return Ok(RouteUpdateResult {
            applied: false,
            route: request.route.clone(),
            detail: "route updates disabled (set PC_AGENT_ENABLE_ROUTE_UPDATES=true to enable)"
                .to_string(),
        });
    }

    let route = request.route.trim();
    if parse_ipv4_cidr(route).is_none() {
        return Err(format!("invalid route '{}': expected IPv4 CIDR", request.route).into());
    }

    let max_nexthops = env_u64("PC_AGENT_ROUTE_UPDATE_MAX_NEXTHOPS", 8) as usize;
    if request.nexthops.is_empty() {
        return Err("route update requires at least one nexthop".into());
    }
    if request.nexthops.len() > max_nexthops {
        return Err(format!(
            "too many nexthops ({}), max allowed is {}",
            request.nexthops.len(),
            max_nexthops
        )
        .into());
    }

    for nh in &request.nexthops {
        if let Some(validation_error) = validate_nexthop(nh) {
            return Err(validation_error.into());
        }
    }

    let nexthops = normalize_nexthops(request.nexthops.clone());
    let min_interval_ms = env_u64("PC_AGENT_ROUTE_UPDATE_MIN_INTERVAL_MS", 1500);
    let min_delta_percent = env_f64("PC_AGENT_ROUTE_UPDATE_MIN_DELTA_PERCENT", 10.0).max(0.0);

    let mut state = route_update_state()
        .lock()
        .map_err(|e| format!("failed to lock route update state: {e}"))?;

    if let Some(last_state) = state.get(route) {
        let elapsed_ms = last_state.last_applied.elapsed().as_millis() as u64;
        if elapsed_ms < min_interval_ms {
            return Ok(RouteUpdateResult {
                applied: false,
                route: route.to_string(),
                detail: format!(
                    "guardrail: skipped update for {} ({}ms since last update, min={}ms)",
                    route, elapsed_ms, min_interval_ms
                ),
            });
        }

        if same_nexthop_set(&last_state.nexthops, &nexthops) {
            let delta = max_relative_weight_delta_percent(&last_state.nexthops, &nexthops);
            if delta < min_delta_percent {
                return Ok(RouteUpdateResult {
                    applied: false,
                    route: route.to_string(),
                    detail: format!(
                        "guardrail: skipped update for {} (max weight delta {:.2}% < min {:.2}%)",
                        route, delta, min_delta_percent
                    ),
                });
            }
        }
    }

    let mut args: Vec<String> = vec![
        "ip".to_string(),
        "route".to_string(),
        "replace".to_string(),
        route.to_string(),
    ];

    if nexthops.len() == 1 {
        let nh = &nexthops[0];
        args.extend([
            "via".to_string(),
            nh.via.clone(),
            "dev".to_string(),
            nh.dev.clone(),
        ]);
    } else {
        for nh in &nexthops {
            args.extend([
                "nexthop".to_string(),
                "via".to_string(),
                nh.via.clone(),
                "dev".to_string(),
                nh.dev.clone(),
                "weight".to_string(),
                nh.weight.to_string(),
            ]);
        }
    }

    let output = Command::new("sudo").args(args.iter()).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!("ip route replace failed: {detail}").into());
    }

    state.insert(
        route.to_string(),
        LastRouteState {
            last_applied: Instant::now(),
            nexthops,
        },
    );

    Ok(RouteUpdateResult {
        applied: true,
        route: route.to_string(),
        detail: "route update applied".to_string(),
    })
}

#[cfg(not(target_os = "linux"))]
pub fn apply_route_update(
    _request: &RouteUpdateRequest,
) -> Result<RouteUpdateResult, Box<dyn std::error::Error>> {
    Err("Route updates are only supported on Linux".into())
}

#[cfg(target_os = "linux")]
fn run_sudo_command_owned(
    args: &[String],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_sudo_command(&arg_refs)?;
    log_command_output("tc class add", &arg_refs, &output);
    Ok(output)
}

#[cfg(target_os = "linux")]
fn htb_burst_bytes(bandwidth: &str) -> u64 {
    const DEFAULT_BPS: u64 = 1_000_000_000;
    const DEFAULT_MIN_BURST_BYTES: u64 = 15_000;
    const BURST_WINDOW_US: u128 = 1_000;
    const USEC_PER_SEC: u128 = 1_000_000;
    const BITS_PER_BYTE: u128 = 8;

    let bps = parse_tc_rate_to_bps(bandwidth).unwrap_or_else(|| {
        warn!(
            "Invalid bandwidth '{bandwidth}', falling back to {DEFAULT_BPS} bps for HTB burst sizing"
        );
        DEFAULT_BPS
    });

    let computed = ((bps as u128) * BURST_WINDOW_US).div_ceil(USEC_PER_SEC * BITS_PER_BYTE);
    let computed = u64::try_from(computed).unwrap_or(u64::MAX);
    computed.max(DEFAULT_MIN_BURST_BYTES)
}

#[cfg(target_os = "linux")]
fn build_htb_class_add_args(
    interface: &str,
    parent: &str,
    classid: &str,
    bandwidth: &str,
    htb_explicit_limits: bool,
) -> Vec<String> {
    let mut args = vec![
        "tc".to_string(),
        "class".to_string(),
        "add".to_string(),
        "dev".to_string(),
        interface.to_string(),
        "parent".to_string(),
        parent.to_string(),
        "classid".to_string(),
        classid.to_string(),
        "htb".to_string(),
        "rate".to_string(),
        bandwidth.to_string(),
    ];

    if htb_explicit_limits {
        let burst = htb_burst_bytes(bandwidth).to_string();
        args.extend([
            "ceil".to_string(),
            bandwidth.to_string(),
            "burst".to_string(),
            burst.clone(),
            "cburst".to_string(),
            burst,
        ]);
    }

    args
}

#[cfg(target_os = "linux")]
pub fn set_network_conditions(
    interfaces: &[String],
    bandwidth_mbit: &str,
    latency_ms: &str,
    loss_percent: &str,
    htb_explicit_limits: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut result = Vec::new();
    result.push(format!(
        "Setting network conditions: {bandwidth_mbit} bandwidth, {latency_ms} latency, {loss_percent} loss on interfaces: {interfaces:?}"
    ));
    result.push(format!(
        "HTB explicit ceil/burst/cburst: {}",
        if htb_explicit_limits {
            "enabled"
        } else {
            "disabled"
        }
    ));

    if htb_explicit_limits {
        let burst_bytes = htb_burst_bytes(bandwidth_mbit);
        result.push(format!(
            "HTB class settings: ceil={bandwidth_mbit}, burst={burst_bytes} bytes, cburst={burst_bytes} bytes"
        ));
    }

    let packet_limit = netem_packet_limit(bandwidth_mbit, latency_ms);
    result.push(format!(
        "Calculated packet limit for netem: {packet_limit} packets (based on bandwidth-delay product)"
    ));

    for interface in interfaces {
        let show_args = ["tc", "qdisc", "show", "dev", interface.as_str()];
        let show_output = run_sudo_command(&show_args)?;
        log_command_output("tc qdisc show", &show_args, &show_output);

        let qdisc_info = String::from_utf8_lossy(&show_output.stdout);

        if qdisc_info.contains("noqueue") || qdisc_info.contains("htb") {
            let _ = Command::new("sudo")
                .args(["tc", "qdisc", "del", "dev", interface, "root"])
                .output()?;
        }

        let root_qdisc_args = [
            "tc",
            "qdisc",
            "add",
            "dev",
            interface.as_str(),
            "root",
            "handle",
            "1:",
            "htb",
            "default",
            "11",
        ];
        let output = run_sudo_command(&root_qdisc_args)?;
        log_command_output("tc qdisc add", &root_qdisc_args, &output);

        let parent_class_args = build_htb_class_add_args(
            interface.as_str(),
            "1:",
            "1:1",
            bandwidth_mbit,
            htb_explicit_limits,
        );
        let _ = run_sudo_command_owned(&parent_class_args)?;

        let leaf_class_args = build_htb_class_add_args(
            interface.as_str(),
            "1:1",
            "1:11",
            bandwidth_mbit,
            htb_explicit_limits,
        );
        let _ = run_sudo_command_owned(&leaf_class_args)?;

        let packet_limit_string = packet_limit.to_string();
        let netem_args = [
            "tc",
            "qdisc",
            "add",
            "dev",
            interface.as_str(),
            "parent",
            "1:11",
            "handle",
            "10:",
            "netem",
            "limit",
            packet_limit_string.as_str(),
            "delay",
            latency_ms,
            "loss",
            loss_percent,
        ];
        let output = run_sudo_command(&netem_args)?;
        log_command_output("tc qdisc add", &netem_args, &output);
    }

    Ok(result)
}

#[cfg(target_os = "linux")]
fn parse_tc_rate_to_bps(rate: &str) -> Option<u64> {
    let s = rate.trim();
    if s.is_empty() {
        return None;
    }

    let idx = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map(|(i, _)| i)
        .unwrap_or_else(|| s.len());

    let (num_str, unit_str) = s.split_at(idx);
    let value: f64 = num_str.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    let unit = unit_str.trim().to_ascii_lowercase();
    let mul: f64 = match unit.as_str() {
        "" | "bit" | "bps" => 1.0,
        "kbit" | "kbps" => 1_000.0,
        "mbit" | "mbps" => 1_000_000.0,
        "gbit" | "gbps" => 1_000_000_000.0,
        "tbit" | "tbps" => 1_000_000_000_000.0,
        _ => return None,
    };

    let bps = (value * mul).round();
    if !bps.is_finite() || bps <= 0.0 {
        return None;
    }

    Some(bps as u64)
}

#[cfg(target_os = "linux")]
fn parse_tc_time_to_micros(time: &str) -> Option<u64> {
    let s = time.trim();
    if s.is_empty() {
        return None;
    }

    let idx = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map(|(i, _)| i)
        .unwrap_or_else(|| s.len());

    let (num_str, unit_str) = s.split_at(idx);
    let value: f64 = num_str.trim().parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }

    let unit = unit_str.trim().to_ascii_lowercase();
    let mul: f64 = match unit.as_str() {
        "" | "us" | "usec" => 1.0,
        "ms" | "msec" => 1_000.0,
        "s" | "sec" => 1_000_000.0,
        _ => return None,
    };

    let us = (value * mul).round();
    if !us.is_finite() || us < 0.0 {
        return None;
    }

    Some(us as u64)
}

#[cfg(target_os = "linux")]
fn netem_packet_limit(bandwidth: &str, delay: &str) -> u32 {
    const DEFAULT_BPS: u64 = 1_000_000_000;
    const DEFAULT_MTU_BYTES: u64 = 1500;
    const DEFAULT_MIN_LIMIT: u64 = 5000;
    const DEFAULT_MAX_LIMIT: u64 = 1_000_000;
    const HEADROOM_NUM: u128 = 3;
    const HEADROOM_DEN: u128 = 2;

    let min_limit = std::env::var("PC_AGENT_NETEM_MIN_PACKET_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MIN_LIMIT);

    let max_limit = std::env::var("PC_AGENT_NETEM_MAX_PACKET_LIMIT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= min_limit)
        .unwrap_or(DEFAULT_MAX_LIMIT.max(min_limit));

    let bps = parse_tc_rate_to_bps(bandwidth).unwrap_or_else(|| {
        warn!("Invalid bandwidth '{bandwidth}', falling back to {DEFAULT_BPS} bps");
        DEFAULT_BPS
    });

    let delay_us = parse_tc_time_to_micros(delay).unwrap_or_else(|| {
        warn!("Invalid delay '{delay}', falling back to 0us");
        0
    });

    let pkt_bits: u128 = (DEFAULT_MTU_BYTES as u128) * 8;

    let bits_in_flight: u128 = {
        const USEC_PER_SEC: u128 = 1_000_000;
        let n = (bps as u128).saturating_mul(delay_us as u128);
        n.div_ceil(USEC_PER_SEC)
    };

    let bits_with_headroom: u128 = bits_in_flight
        .saturating_mul(HEADROOM_NUM)
        .div_ceil(HEADROOM_DEN);

    let computed_packets: u64 = if bits_with_headroom == 0 {
        0
    } else {
        let p = bits_with_headroom.div_ceil(pkt_bits);
        u64::try_from(p).unwrap_or(u64::MAX)
    };

    let final_limit = computed_packets.max(min_limit).min(max_limit);
    u32::try_from(final_limit).unwrap_or(u32::MAX)
}

#[cfg(not(target_os = "linux"))]
pub fn set_network_conditions(
    _interfaces: &[String],
    _bandwidth_mbit: &str,
    _latency_ms: &str,
    _loss_percent: &str,
    _htb_explicit_limits: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Err("Setting network conditions is only supported on Linux".into())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{build_htb_class_add_args, htb_burst_bytes};

    #[cfg(target_os = "linux")]
    use super::{
        canonicalize_mac, find_virtualwall_manifest_interface, resolve_current_virtualwall_client_id,
        resolve_manifest_interface_names,
    };

    #[cfg(target_os = "linux")]
    const VIRTUALWALL_MANIFEST: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/multipathxr-meta.private.rspec"
    ));

    #[test]
    fn htb_burst_bytes_uses_floor_for_low_bandwidth() {
        assert_eq!(htb_burst_bytes("20mbit"), 15_000);
    }

    #[test]
    fn build_htb_class_add_args_includes_explicit_limits_when_enabled() {
        let args = build_htb_class_add_args("eth1", "1:", "1:1", "20mbit", true);

        assert_eq!(
            args,
            vec![
                "tc", "class", "add", "dev", "eth1", "parent", "1:", "classid", "1:1", "htb",
                "rate", "20mbit", "ceil", "20mbit", "burst", "15000", "cburst", "15000",
            ]
        );
    }

    #[test]
    fn build_htb_class_add_args_can_disable_explicit_limits() {
        let args = build_htb_class_add_args("eth1", "1:", "1:1", "20mbit", false);

        assert_eq!(
            args,
            vec![
                "tc", "class", "add", "dev", "eth1", "parent", "1:", "classid", "1:1", "htb",
                "rate", "20mbit",
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn canonicalize_mac_accepts_virtualwall_formats() {
        assert_eq!(
            canonicalize_mac("00:25:90:01:d4:b1").as_deref(),
            Some("00259001d4b1")
        );
        assert_eq!(canonicalize_mac("00259001d4b1").as_deref(), Some("00259001d4b1"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_virtualwall_graph_hint_to_live_interface_name() {
        let manifest_interface = find_virtualwall_manifest_interface(
            VIRTUALWALL_MANIFEST,
            "regional-a1:if2",
            None,
        )
        .expect("graph hint should resolve in manifest");

        assert_eq!(manifest_interface.component_device.as_deref(), Some("eth1"));
        assert_eq!(manifest_interface.mac.as_deref(), Some("00259001d4b1"));
        assert_eq!(manifest_interface.ipv4_address.as_deref(), Some("192.168.30.10"));

        let mut mac_to_interface = HashMap::new();
        mac_to_interface.insert("00259001d4b1".to_string(), "enp3s0f1".to_string());

        let resolved = resolve_manifest_interface_names(
            &manifest_interface,
            &mac_to_interface,
            &HashMap::new(),
        );
        assert_eq!(resolved, vec!["enp3s0f1".to_string()]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_virtualwall_component_device_for_current_client() {
        let manifest_interface = find_virtualwall_manifest_interface(
            VIRTUALWALL_MANIFEST,
            "eth1",
            Some("regional-a1"),
        )
        .expect("component device should resolve for the current Virtual Wall client");

        assert_eq!(manifest_interface.client_id, "regional-a1:if2");
        assert_eq!(manifest_interface.ipv4_address.as_deref(), Some("192.168.30.10"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_virtualwall_client_id_from_physical_hostname() {
        let candidates = vec!["n084-16".to_string()];
        assert_eq!(
            resolve_current_virtualwall_client_id(VIRTUALWALL_MANIFEST, &candidates).as_deref(),
            Some("regional-a1")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recognizes_missing_qdisc_delete_errors() {
        assert!(super::expected_missing_qdisc_detail(
            "RTNETLINK answers: No such file or directory"
        ));
        assert!(super::expected_missing_qdisc_detail(
            "Error: Cannot delete qdisc with handle of zero."
        ));
        assert!(!super::expected_missing_qdisc_detail("permission denied"));
    }
}

pub fn extract_port_from_url(url: &str) -> Option<u16> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let scheme_end = trimmed.find("://")?;
    let scheme = &trimmed[..scheme_end];
    let rest = &trimmed[scheme_end + 3..];

    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return default_port_for_scheme(scheme);
    }

    if authority.starts_with('[') {
        let closing = authority.find(']')?;
        let after_bracket = &authority[closing + 1..];
        if let Some(port_str) = after_bracket.strip_prefix(':') {
            return port_str.parse::<u16>().ok();
        }
        return default_port_for_scheme(scheme);
    }

    if let Some(idx) = authority.rfind(':') {
        let host_part = &authority[..idx];
        let port_part = &authority[idx + 1..];
        if !host_part.is_empty()
            && !port_part.is_empty()
            && port_part.chars().all(|c| c.is_ascii_digit())
        {
            return port_part.parse::<u16>().ok();
        }
    }

    default_port_for_scheme(scheme)
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" | "moq" => Some(443),
        _ => None,
    }
}

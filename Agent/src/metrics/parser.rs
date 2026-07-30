use serde::Serialize;
use std::collections::BTreeMap;

/// One parsed Prometheus sample line.
///
/// This intentionally stores labels and values in a normalized form that is
/// useful for later forwarding, aggregation, or filtering.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrometheusSample {
    /// Metric name, for example `process_cpu_seconds_total`.
    pub metric_name: String,
    /// Parsed label set.
    pub labels: BTreeMap<String, String>,
    /// Parsed value.
    pub value: AgentSampleValue,
    /// Optional explicit sample timestamp from the exposition line.
    pub timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AgentSampleValue {
    Float(f64),
    Text(String),
}

/// Parsing result for an entire `/metrics` document.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseReport {
    /// Successfully parsed samples.
    pub samples: Vec<PrometheusSample>,
    /// Number of malformed non-comment metric lines skipped.
    pub malformed_lines: usize,
}

pub fn inject_agent_labels(
    sample: &PrometheusSample,
    agent_node_id: &str,
    source_ip: &str,
    source_port: u16,
) -> PrometheusSample {
    let mut labels = sample.labels.clone();

    labels
        .entry("agent_node_id".to_string())
        .or_insert_with(|| agent_node_id.to_string());

    labels
        .entry("agent_source_ip".to_string())
        .or_insert_with(|| source_ip.to_string());

    labels
        .entry("agent_source_port".to_string())
        .or_insert_with(|| source_port.to_string());

    labels
        .entry("agent_source_instance".to_string())
        .or_insert_with(|| format!("{agent_node_id}@{source_ip}:{source_port}"));

    // Preserve original `instance` if present; do not modify it.
    PrometheusSample {
        metric_name: sample.metric_name.clone(),
        labels,
        value: sample.value.clone(),
        timestamp_ms: sample.timestamp_ms,
    }
}

/// Parses Prometheus text exposition into a lightweight in-memory representation.
///
/// This parser is intentionally permissive:
/// - comment lines are ignored
/// - blank lines are ignored
/// - malformed sample lines are counted and skipped
///
/// It supports the common form:
/// `metric_name{label="value"} 123.4 1710000000000`
pub fn parse_prometheus_text(input: &str) -> ParseReport {
    let mut samples = Vec::new();
    let mut malformed_lines = 0usize;

    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        match parse_sample_line(line) {
            Some(sample) => samples.push(sample),
            None => malformed_lines = malformed_lines.saturating_add(1),
        }
    }

    ParseReport {
        samples,
        malformed_lines,
    }
}

fn parse_sample_line(line: &str) -> Option<PrometheusSample> {
    let (metric_and_labels, value_and_ts) = split_once_ascii_whitespace(line)?;
    let (metric_name, labels) = parse_metric_and_labels(metric_and_labels)?;
    let mut rest = value_and_ts.split_ascii_whitespace();

    let raw_value = rest.next()?;
    let value = parse_sample_value(raw_value)?;

    let timestamp_ms = match rest.next() {
        Some(raw_ts) => raw_ts.parse::<i64>().ok(),
        None => None,
    };

    Some(PrometheusSample {
        metric_name,
        labels,
        value,
        timestamp_ms,
    })
}

fn split_once_ascii_whitespace(input: &str) -> Option<(&str, &str)> {
    let idx = input.find(|c: char| c.is_ascii_whitespace())?;
    let left = &input[..idx];
    let right = input[idx..].trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some((left, right))
}

fn parse_metric_and_labels(input: &str) -> Option<(String, BTreeMap<String, String>)> {
    if let Some(open) = input.find('{') {
        let close = input.rfind('}')?;
        if close <= open {
            return None;
        }

        let metric_name = input[..open].trim();
        if metric_name.is_empty() {
            return None;
        }

        let labels_raw = &input[open + 1..close];
        let labels = parse_labels(labels_raw)?;
        return Some((metric_name.to_string(), labels));
    }

    if input.trim().is_empty() {
        return None;
    }

    Some((input.trim().to_string(), BTreeMap::new()))
}

fn parse_labels(input: &str) -> Option<BTreeMap<String, String>> {
    let mut labels = BTreeMap::new();
    let bytes = input.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let key_start = i;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '=' || c.is_ascii_whitespace() {
                break;
            }
            i += 1;
        }

        let key = input[key_start..i].trim();
        if key.is_empty() {
            return None;
        }

        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] as char != '=' {
            return None;
        }
        i += 1;

        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] as char != '"' {
            return None;
        }
        i += 1;

        let mut value = String::new();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c == '\\' {
                i += 1;
                if i >= bytes.len() {
                    return None;
                }
                let escaped = bytes[i] as char;
                match escaped {
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    'n' => value.push('\n'),
                    other => value.push(other),
                }
                i += 1;
                continue;
            }

            if c == '"' {
                i += 1;
                break;
            }

            value.push(c);
            i += 1;
        }

        labels.insert(key.to_string(), value);

        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }

        if i < bytes.len() {
            if bytes[i] as char != ',' {
                return None;
            }
            i += 1;
        }
    }

    Some(labels)
}

fn parse_sample_value(input: &str) -> Option<AgentSampleValue> {
    match input {
        "+Inf" | "Inf" => Some(AgentSampleValue::Float(f64::INFINITY)),
        "-Inf" => Some(AgentSampleValue::Float(f64::NEG_INFINITY)),
        "NaN" => Some(AgentSampleValue::Float(f64::NAN)),
        other => {
            if let Ok(value) = other.parse::<f64>() {
                return Some(AgentSampleValue::Float(value));
            }
            Some(AgentSampleValue::Text(other.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_sample() {
        let report = parse_prometheus_text("up 1\n");
        assert_eq!(report.malformed_lines, 0);
        assert_eq!(report.samples.len(), 1);
        assert_eq!(report.samples[0].metric_name, "up");
        assert_eq!(report.samples[0].timestamp_ms, None);
        assert_eq!(report.samples[0].labels.len(), 0);
    }

    #[test]
    fn parses_labels_and_timestamp() {
        let report = parse_prometheus_text(
            "http_requests_total{method=\"GET\",path=\"/metrics\"} 42 1710000000000\n",
        );
        assert_eq!(report.malformed_lines, 0);
        assert_eq!(report.samples.len(), 1);

        let sample = &report.samples[0];
        assert_eq!(sample.metric_name, "http_requests_total");
        assert_eq!(sample.labels.get("method").map(String::as_str), Some("GET"));
        assert_eq!(
            sample.labels.get("path").map(String::as_str),
            Some("/metrics")
        );
        assert_eq!(sample.timestamp_ms, Some(1710000000000));
    }

    #[test]
    fn ignores_comments_and_counts_malformed_lines() {
        let report = parse_prometheus_text(
            "# HELP something\n# TYPE something counter\nvalid_metric 1\nbroken line {\n",
        );
        assert_eq!(report.samples.len(), 1);
        assert_eq!(report.malformed_lines, 1);
    }
}

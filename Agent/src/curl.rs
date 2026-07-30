use crate::logging::emit_log;
use rust_socketio::{Payload, RawClient};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::error;

#[derive(Debug, Deserialize)]
struct CurlRequest {
    url: String,
    #[serde(default = "default_curl_method")]
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default = "default_curl_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_curl_response_body_limit")]
    response_body_limit: usize,
}

fn default_curl_method() -> String {
    "GET".to_string()
}

fn default_curl_timeout_ms() -> u64 {
    30_000
}

fn default_curl_response_body_limit() -> usize {
    16 * 1024
}

pub fn handle_curl_event(payload: Payload, socket: RawClient, ack: i32) {
    match parse_curl_request(payload) {
        Ok(request) => {
            emit_log(
                &socket,
                "info",
                true,
                &format!(
                    "Executing curl event: method={} url={}",
                    request.method, request.url
                ),
            );

            match execute_curl_request(&request) {
                Ok(response_payload) => {
                    emit_log(
                        &socket,
                        "info",
                        true,
                        &format!(
                            "curl request completed successfully: method={} url={}",
                            request.method, request.url
                        ),
                    );
                    let _ = socket.ack(ack, response_payload.clone());
                    if let Err(e) = socket.emit("curl_result", response_payload) {
                        error!("Failed to emit curl_result: {}", e);
                    }
                }
                Err(e) => {
                    let error_payload = json!({
                        "status": "error",
                        "message": e,
                    });
                    emit_log(
                        &socket,
                        "error",
                        true,
                        &format!("curl request failed: {error_payload}"),
                    );
                    let _ = socket.ack(ack, error_payload.clone());
                    if let Err(emit_err) = socket.emit("curl_result", error_payload) {
                        error!("Failed to emit curl_result error payload: {}", emit_err);
                    }
                }
            }
        }
        Err(e) => {
            let error_payload = json!({
                "status": "error",
                "message": e,
            });
            emit_log(
                &socket,
                "error",
                true,
                &format!("Invalid curl payload: {error_payload}"),
            );
            let _ = socket.ack(ack, error_payload.clone());
            if let Err(emit_err) = socket.emit("curl_result", error_payload) {
                error!("Failed to emit curl_result invalid payload: {}", emit_err);
            }
        }
    }
}

fn parse_curl_request(payload: Payload) -> Result<CurlRequest, String> {
    match payload {
        Payload::Text(values) => parse_curl_request_from_values(&values),
        Payload::Binary(_) => Err("Binary curl payloads are not supported".to_string()),
        _ => Err("Unsupported curl payload type".to_string()),
    }
}

fn parse_curl_request_from_values(values: &[Value]) -> Result<CurlRequest, String> {
    if values.is_empty() {
        return Err("curl payload is empty".to_string());
    }

    if values.len() == 1 {
        match &values[0] {
            Value::String(url) => {
                return Ok(CurlRequest {
                    url: url.clone(),
                    method: default_curl_method(),
                    headers: BTreeMap::new(),
                    body: None,
                    timeout_ms: default_curl_timeout_ms(),
                    response_body_limit: default_curl_response_body_limit(),
                });
            }
            Value::Object(_) => {
                return serde_json::from_value(values[0].clone())
                    .map_err(|e| format!("Failed to deserialize curl request object: {e}"));
            }
            _ => {}
        }
    }

    let url = values
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "Expected curl payload format [url, method?] or { url, ... }".to_string())?
        .to_string();

    let method = values
        .get(1)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(default_curl_method);

    Ok(CurlRequest {
        url,
        method,
        headers: BTreeMap::new(),
        body: None,
        timeout_ms: default_curl_timeout_ms(),
        response_body_limit: default_curl_response_body_limit(),
    })
}

fn execute_curl_request(request: &CurlRequest) -> Result<Value, String> {
    let method = reqwest::Method::from_bytes(request.method.trim().as_bytes())
        .map_err(|e| format!("Invalid HTTP method '{}': {e}", request.method))?;

    let timeout_ms = request.timeout_ms.clamp(1, 300_000);
    let body_limit = request.response_body_limit.clamp(1, 1024 * 1024);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;

    let mut builder = client.request(method, &request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }

    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }

    let response = builder
        .send()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let body_text = response
        .text()
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    let truncated = body_text.chars().count() > body_limit;
    let response_body = if truncated {
        body_text.chars().take(body_limit).collect::<String>()
    } else {
        body_text
    };

    Ok(json!({
        "status": "ok",
        "body": response_body,
    }))
}

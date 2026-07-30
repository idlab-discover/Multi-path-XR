use once_cell::sync::Lazy;
use regex::Regex;
use rust_socketio::RawClient;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{error, Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

static ANSI_ESCAPE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap_or_else(|e| {
        error!("Failed to compile regex: {}", e);
        Regex::new("").unwrap()
    })
});

static APPLICATION_LOG_CLIENT: Lazy<Mutex<Option<Arc<Mutex<rust_socketio::client::Client>>>>> =
    Lazy::new(|| Mutex::new(None));

struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }
}

pub struct ApplicationLoggingLayer {
    pub log_level: Level,
}

impl<S> Layer<S> for ApplicationLoggingLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let event_level = *event.metadata().level();
        if event_level < self.log_level {
            return;
        }

        let mut visitor = MessageVisitor { message: None };
        event.record(&mut visitor);

        let message = visitor
            .message
            .unwrap_or_else(|| event.metadata().name().to_string());
        let level = event_level.as_str();
        let location = match (event.metadata().file(), event.metadata().line()) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            (Some(file), None) => file.to_string(),
            _ => event.metadata().target().to_string(),
        };

        log_to_application(&message, &level, &location);
    }
}

pub fn set_application_log_client(client: Arc<Mutex<rust_socketio::client::Client>>) {
    if let Ok(mut guard) = APPLICATION_LOG_CLIENT.lock() {
        *guard = Some(client);
    }
}

pub fn emit_log(socket: &RawClient, level: &str, agent_tag: bool, data: &str) {
    let sanitized_data = sanitize_log(data);
    let payload = if agent_tag {
        json!({ "level": level, "data": format!("[agent] {}", sanitized_data) })
    } else {
        json!({ "level": level, "data": sanitized_data })
    };
    if let Err(e) = socket.emit("process_output", payload) {
        error!("Failed to emit log: {}", e);
    }
}

fn sanitize_log(data: &str) -> String {
    ANSI_ESCAPE
        .replace_all(data, "")
        .replace(|c: char| c.is_control(), "")
        .to_string()
}

fn log_to_application(message: &str, level: &str, location: &str) {
    let client = match APPLICATION_LOG_CLIENT.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => None,
    };

    if let Some(client) = client {
        let payload = json!({
            "level": level.trim().to_ascii_lowercase(),
            "data": format!("[agent] {}", sanitize_log(message)),
            "location": sanitize_log(location),
        });

        match client.lock() {
            Ok(client_lock) => {
                if let Err(err) = client_lock.emit("process_output", payload) {
                    eprintln!("Failed to emit application log: {err}");
                }
            }
            Err(_) => {
                eprintln!("Failed to acquire websocket client lock for application logging");
            }
        }
    }
}

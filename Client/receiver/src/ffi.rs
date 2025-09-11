use std::ffi::CString;
use std::sync::{Arc, RwLock};
use interoptopus::{ffi_function, function, callback, Inventory, InventoryBuilder};
use interoptopus::patterns::{slice::FFISlice, string::AsciiPointer};
use tracing::level_filters::LevelFilter;
use tracing::{warn, Level};
use tracing::{Event, Subscriber, error, info, field::{Field, Visit}};
use tracing_subscriber::{layer::Context, Layer, registry::LookupSpan, prelude::*};
use once_cell::sync::Lazy;
use crate::ingress::Ingress;
use crate::types::{DataCallback, FrameData};
use crate::utils::{create_metrics, start_metrics_server, stop_metrics_server};
#[cfg(feature = "console-tracing")]
use std::time::Duration;

/// Returns the version of this API.
#[ffi_function]
#[no_mangle]
pub extern "C" fn version() -> u32 {
    0x00_02_00_00
}

// Define a callback type for logging messages to the application that uses this library.
callback!(DebugCallback(message: AsciiPointer, log_level: AsciiPointer));

// Use a static mutable callback for logging messages to the application that uses this library.
static DEBUG_CALLBACK: Lazy<Arc<RwLock<Option<DebugCallback>>>> = Lazy::new(|| Arc::new(RwLock::new(None)));

/// Registers a callback for logging messages to the application that uses this library.
#[ffi_function]
#[no_mangle]
pub extern "C" fn register_debug_callback(callback: DebugCallback) {
    let mut callback_guard = DEBUG_CALLBACK.write().unwrap();
    *callback_guard = Some(callback);
}

#[ffi_function]
#[no_mangle]
pub extern "C" fn unregister_debug_callback() {
    let mut callback_guard = DEBUG_CALLBACK.write().unwrap();
    *callback_guard = None;
}

/// Logs a message to the application if a callback is registered.
fn log_to_application(message: &str, log_level: &str, location: &str) {
    let callback_option = DEBUG_CALLBACK.read().unwrap().clone();
    if let Some(ref callback) = callback_option {
        // If the message is empty or equal to "No message", don't log it
        if message.is_empty() || message == "No message" {
            return;
        }
        let full_message = format!("{message}\n{location}");
        // Convert the message to a CString
        if let Ok(c_message) = CString::new(full_message) {
            let c_log_level = CString::new(log_level).unwrap_or_else(|_| CString::new("INFO").unwrap());
            callback.call(
                AsciiPointer::from_cstr(c_message.as_c_str()),
                AsciiPointer::from_cstr(c_log_level.as_c_str()),
            );
        } else {
            eprintln!("Failed to convert message to CString: Message contains interior null byte");
        }
    }
}

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
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

// Custom tracing Layer that forwards logs to Unity Debug.Log.
pub struct ApplicationLoggingLayer {
    pub log_level: Level,
}

impl<S> Layer<S> for ApplicationLoggingLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event, _ctx: Context<S>) {
        let log_level = *event.metadata().level();
        // Ignore logs that are below the log level
        if log_level < self.log_level {
            return;
        }

        let mut visitor = MessageVisitor { message: None };
        event.record(&mut visitor);

        let message = visitor.message.unwrap_or_else(|| "No message".to_string());
        let log_level = format!("{log_level:?}");
        let location = event.metadata().name().to_string();

        log_to_application(&message, &log_level, &location);
    }
}

// Use a static mutable Ingress wrapped in an Arc<RwLock> for safe concurrent access
static INGRESS_INSTANCE: Lazy<Arc<RwLock<Option<Ingress>>>> = Lazy::new(|| Arc::new(RwLock::new(None)));

#[ffi_function]
#[no_mangle]
pub extern "C" fn init(
    log_level: u32,
    server_url: AsciiPointer,
    multicast_url: AsciiPointer,
) {
    {
        let ingress_guard = INGRESS_INSTANCE.read().unwrap();
        if ingress_guard.is_some() {
            error!("Ingress already started");
            return;
        }
    }

    // Map the LogLevel enum to the LevelFilter enum
    let log_level = match log_level {
        0 => LevelFilter::TRACE,
        1 => LevelFilter::DEBUG,
        2 => LevelFilter::INFO,
        3 => LevelFilter::WARN,
        4 => LevelFilter::ERROR,
        _ => LevelFilter::INFO,
    };

    // Convert the server_url and multicast_url to Rust strings and create a copy
    let server_url = server_url.as_str().unwrap_or("http://localhost:3001").to_string();
    let multicast_url = multicast_url.as_str().unwrap_or("udp://239.0.0.1:40085").to_string();

    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(log_level);

    let app_layer = ApplicationLoggingLayer { log_level: log_level.into_level().unwrap_or(Level::INFO)  }.with_filter(log_level);


    #[cfg(feature = "console-tracing")]
    let subscriber = {
        let console_layer = console_subscriber::ConsoleLayer::builder()
            .retention(Duration::from_secs(60))
            .server_addr(([127, 0, 0, 1], 5555))
            .spawn();
        tracing_subscriber::registry()
            .with(console_layer)
            .with(fmt_layer)
            .with(app_layer)
    };

    #[cfg(not(feature = "console-tracing"))]
    let subscriber = {
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(app_layer)
    };

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    create_metrics().unwrap();

    let ingress = Ingress::new(10, false);
    // Set the parameters first before initializing
    {
        let stream_manager = ingress.get_stream_manager();
        stream_manager.set_websocket_url(server_url);
        stream_manager.set_flute_url(multicast_url);
    }
    // Finish initializing the ingress system

    ingress.initialize();

    {
        let mut w = INGRESS_INSTANCE.write().unwrap();
        *w = Some(ingress);
    }

    start_metrics_server(3380);

    info!("Receiver client library initialized");
}

#[ffi_function]
#[no_mangle]
pub extern "C" fn destroy() {

    /*
    let mut ingress_guard = INGRESS_INSTANCE.lock().unwrap();
    if let Some(ref ingress) = *ingress_guard {
        // Clean up the ingress instance
        ingress.cleanup();
        *ingress_guard = None;
    }
    */
    let _ = stop_metrics_server();

    {
        let mut ingress_guard = INGRESS_INSTANCE.write().unwrap();
        if let Some(ingress) = ingress_guard.take() {
            ingress.stop();
            info!("All ingresses have stopped");
            ingress.get_storage().reset();
        }

        // Clear the ingress instance
        *ingress_guard = None;
    }
    info!("Receiver client library destroyed");

    // Unsubscribe from the subscription callback
    {
        SUBSCRIPTION_CALLBACK.write().unwrap().take();
    }
    info!("Subscription callback unregistered");

    // Unregister the debug callback
    {
        DEBUG_CALLBACK.write().unwrap().take();
    }
    info!("Debug callback unregistered");
}

/// Get the list of stream IDs.
/// This function returns all active stream IDs as a vector of strings.
#[ffi_function]
#[no_mangle]
pub extern "C" fn get_stream_ids(callback: extern "C" fn(*const std::os::raw::c_char)) {
    let joined = {
        let r = INGRESS_INSTANCE.read().unwrap();
        if let Some(ref ingress) = *r {
            ingress.get_storage().get_stream_ids().join(",")
        } else {
            String::new()
        }
    };
    if !joined.is_empty() {
        let c_string = std::ffi::CString::new(joined).unwrap();
        callback(c_string.as_ptr());
        // NOTE: callback must copy the string synchronously.
    }
}

callback!(SubscriptionCallback(
    send_time: u64,
    presentation_time: u64,
    error_count: u64,
    point_count: u64,
    coordinates: FFISlice<f32>,
    colors: FFISlice<u8>,
    stream_id: AsciiPointer
));

static SUBSCRIPTION_CALLBACK: Lazy<Arc<RwLock<Option<DataCallback>>>> =
    Lazy::new(|| Arc::new(RwLock::new(None)));

#[ffi_function]
#[no_mangle]
pub extern "C" fn ingress_subscribe(callback: SubscriptionCallback) {
    let rust_callback = move |frame_data: FrameData, stream_id: String| {
        let c_stream_id = std::ffi::CString::new(stream_id).unwrap_or_else(|_| std::ffi::CString::new("invalid").unwrap());
        callback.call(
            frame_data.send_time,
            frame_data.presentation_time,
            frame_data.error_count,
            frame_data.point_count,
            frame_data.coordinates.as_slice().into(),
            frame_data.colors.as_slice().into(),
            AsciiPointer::from_cstr(c_stream_id.as_c_str()),
        );
    };
    // Save the callback in a global variable to keep it alive
    let mut subscription_callback_guard = SUBSCRIPTION_CALLBACK.write().unwrap();
    *subscription_callback_guard = Some(Arc::new(rust_callback) as Arc<dyn Fn(FrameData, String) + Send + Sync>);
}

#[ffi_function]
#[no_mangle]
pub extern "C" fn ingress_unsubscribe() {
    // Remove the callback from the global variable
    SUBSCRIPTION_CALLBACK.write().unwrap().take();
}

#[ffi_function]
#[no_mangle]
pub extern "C" fn consume_frame(stream_id: FFISlice<u8>) -> bool {
    let stream_id_str = match String::from_utf8(stream_id.as_slice().to_vec()) {
        Ok(id) => id,
        Err(_) => return false,
    };

    let frame_opt = {
        let ingress_guard = INGRESS_INSTANCE.read().unwrap();
        if let Some(ref ingress) = *ingress_guard {
            ingress.get_storage().consume_frame(&stream_id_str)
        } else {
            None
        }
    };

    let frame_data = match frame_opt {
        Some(f) => f,
        None => return false,
    };

    let cb_opt = {
        let guard = SUBSCRIPTION_CALLBACK.read().unwrap();
        guard.clone()
    };
    let cb = match cb_opt {
        Some(cb) => cb,
        None => return false,
    };

    cb(frame_data, stream_id_str.clone());
    true
}

#[ffi_function]
#[no_mangle]
pub extern "C" fn dash_set_fetching_enabled(
    group_id: AsciiPointer,
    enabled: bool,
) {
    let group_id = group_id.as_str().unwrap_or("");
    if group_id.is_empty() {
        warn!("Group ID is empty, cannot toggle fetching.");
        return;
    }

    let ingress_guard = INGRESS_INSTANCE.read().unwrap();
    if let Some(ref ingress) = *ingress_guard {
        if let Some(dash) = ingress.get_stream_manager().dash_ingress.read().unwrap().as_ref() {
            dash.set_fetching_enabled(group_id, enabled);
        } else {
            warn!("Dash ingress not initialized.");
        }
    } else {
        error!("Cannot toggle fetching - ingress instance not initialized");
    }
}

pub fn build_binding_inventory() -> Inventory {
    InventoryBuilder::new()
        .register(function!(version))
        .register(function!(register_debug_callback))
        .register(function!(unregister_debug_callback))
        .register(function!(init))
        .register(function!(destroy))
        .register(function!(get_stream_ids))
        .register(function!(ingress_subscribe))
        .register(function!(ingress_unsubscribe))
        .register(function!(consume_frame))
        .register(function!(dash_set_fetching_enabled))
        .inventory()
}

use prometheus::{self, Gauge, IntGauge, Opts, Registry};
use sysinfo::{System, Networks};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};
use tracing::{debug, instrument};
use once_cell::sync::Lazy;

/// Global singleton for the `Metrics` instance.
pub static METRICS: Lazy<Arc<Mutex<Option<Metrics>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

/// Metrics struct to manage CPU, memory, and network metrics.
#[derive(Debug, Clone)]
pub struct Metrics {
    registry: Registry,
    common_labels: Arc<RwLock<Vec<(String, String)>>>, // Switched to RwLock for read-heavy workloads
    cpu_usage: Gauge,
    memory_usage: Gauge,
    network_metrics: Vec<(String, Gauge, Gauge)>, // (Interface, RX, TX)
    custom_gauges: Arc<Mutex<HashMap<String, IntGauge>>>, // Store custom gauges by name
    system: Arc<Mutex<System>>,
    networks: Arc<Mutex<Networks>>,
}

pub struct MetricsBuilder {
    interfaces: Vec<String>,
    common_labels: Vec<(String, String)>,
    custom_gauges: HashMap<String, Opts>, // Custom gauges to be added
}

impl MetricsBuilder {
    /// Create a new `MetricsBuilder`.
    #[instrument(skip_all)]
    pub fn new() -> Self {
        Self {
            interfaces: Vec::new(),
            common_labels: Vec::new(),
            custom_gauges: HashMap::new(),
        }
    }

    /// Add a network interface to track.
    #[instrument(skip_all)]
    pub fn track_interface(mut self, interface: &str) -> Self {
        self.interfaces.push(interface.to_string());
        self
    }

    /// Add a common label to be applied to all metrics.
    #[instrument(skip_all)]
    pub fn add_label(mut self, key: &str, value: &str) -> Self {
        self.common_labels.push((key.to_string(), value.to_string()));
        self
    }

    /// Add a gauge by name and description.
    #[instrument(skip_all)]
    pub fn add_gauge(mut self, name: &str, description: &str) -> Self {
        let opts = Self::opts_with_labels(name, description, &self.common_labels);
        self.custom_gauges.insert(name.to_string(), opts);
        self
    }

    /// Build the Metrics struct.
    #[instrument(skip_all)]
    pub fn build(self) -> Metrics {
        let registry = Registry::new();

        let cpu_usage = Gauge::with_opts(Self::opts_with_labels(
            "cpu_usage",
            "CPU usage percentage",
            &self.common_labels,
        ))
        .expect("Failed to create CPU usage gauge");
        let memory_usage = Gauge::with_opts(Self::opts_with_labels(
            "memory_usage",
            "Memory usage in bytes",
            &self.common_labels,
        ))
        .expect("Failed to create memory usage gauge");

        registry.register(Box::new(cpu_usage.clone())).expect("Failed to register CPU usage gauge");
        registry
            .register(Box::new(memory_usage.clone()))
            .expect("Failed to register memory usage gauge");

        let mut network_metrics = Vec::new();
        for interface in self.interfaces {
            let sanitized_interface = Self::sanitize_name(&interface);
            let rx = Gauge::with_opts(Self::opts_with_labels(
                &format!("{}_rx_bytes", sanitized_interface),
                &format!("Received bytes for {}", interface),
                &self.common_labels,
            ))
            .expect("Failed to create RX gauge");
            let tx = Gauge::with_opts(Self::opts_with_labels(
                &format!("{}_tx_bytes", sanitized_interface),
                &format!("Transmitted bytes for {}", interface),
                &self.common_labels,
            ))
            .expect("Failed to create TX gauge");

            registry.register(Box::new(rx.clone())).expect("Failed to register RX gauge");
            registry.register(Box::new(tx.clone())).expect("Failed to register TX gauge");

            network_metrics.push((interface, rx, tx)); // Store the non-sanitized interface name, needed for sysinfo lookups (in the update method)
        }

        let mut custom_gauges = HashMap::new();
        for (name, opts) in self.custom_gauges {
            let gauge = IntGauge::with_opts(opts).expect("Failed to create custom gauge");
            registry.register(Box::new(gauge.clone())).expect("Failed to register custom gauge");
            custom_gauges.insert(name, gauge);
        }

        debug!("Metrics successfully built");

        let metrics = Metrics {
            registry,
            common_labels: Arc::new(RwLock::new(self.common_labels)),
            cpu_usage,
            memory_usage,
            network_metrics,
            custom_gauges: Arc::new(Mutex::new(custom_gauges)),
            system: Arc::new(Mutex::new(System::new())),
            networks: Arc::new(Mutex::new(Networks::new_with_refreshed_list())),
        };



        let mut metrics_guard = METRICS.lock().unwrap();
        // Register the instance
        if metrics_guard.is_some() {
            panic!("Metrics instance already initialized.");
        }

        *metrics_guard = Some(metrics);

        // Now return the instance
        (*metrics_guard.as_ref().unwrap()).clone()
    }

    /// Sanitize interface names to create valid Prometheus metric names.
    #[instrument(skip_all)]
    fn sanitize_name(name: &str) -> String {
        name
        // Replace all non alphanumeric characters with underscores
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect()
    }

    /// Helper to create metric options with labels.
    #[instrument(skip_all)]
    fn opts_with_labels(name: &str, help: &str, labels: &[(String, String)]) -> Opts {
        let mut opts = Opts::new(name, help);
        for (key, value) in labels {
            opts = opts.const_label(key.clone(), value.clone());
        }
        opts
    }
}

/// Retrieve the global Metrics instance.
#[instrument(skip_all)]
pub fn get_metrics() -> Metrics {
    let metrics_guard = METRICS.lock().unwrap();
    if let Some(ref metrics) = *metrics_guard {
        return metrics.clone();
    }

    panic!("Metrics instance not initialized. Create a MetricsBuilder and call build().");
}


impl Metrics {
    /// Update metrics.
    #[instrument(skip_all)]
    pub fn update(&self) {
        let mut sys = self.system.lock().expect("Failed to lock system data");
        sys.refresh_all();

        let cpu_usage = sys.global_cpu_usage();
        let memory_usage = sys.used_memory() as f64;

        self.cpu_usage.set(cpu_usage as f64);
        self.memory_usage.set(memory_usage);

        if self.network_metrics.is_empty() {
            return;
        }

        let mut networks = self.networks.lock().expect("Failed to lock network data");
        networks.refresh(true);

        for (interface, rx, tx) in &self.network_metrics {
            if let Some(data) = networks.get(interface) {
                rx.set(data.total_received() as f64);
                tx.set(data.total_transmitted() as f64);
            }
        }
    }

    /// Add or get a custom gauge by name.
    #[instrument(skip_all)]
    pub fn get_or_create_gauge(&self, name: &str, description: &str) -> Result<IntGauge, String> {
        let mut gauges = self
            .custom_gauges
            .lock()
            .map_err(|_| "Failed to lock custom gauges".to_string())?;
        if let Some(gauge) = gauges.get(name) {
            return Ok(gauge.clone());
        }

        let labels = self
            .common_labels
            .read()
            .map_err(|_| "Failed to lock common labels".to_string())?;
        let opts = MetricsBuilder::opts_with_labels(name, description, &labels);
        let gauge = IntGauge::with_opts(opts).map_err(|e| format!("Failed to create gauge: {}", e))?;
        self.registry
            .register(Box::new(gauge.clone()))
            .map_err(|e| format!("Failed to register gauge: {}", e))?;
        gauges.insert(name.to_string(), gauge.clone());
        Ok(gauge)
    }

    /// Get the Prometheus registry.
    #[instrument(skip_all)]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}
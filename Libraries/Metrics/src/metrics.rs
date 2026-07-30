use dashmap::{mapref::entry::Entry, DashMap};
use once_cell::sync::Lazy;
use prometheus::{self, Gauge, IntGauge, IntGaugeVec, Opts, Registry};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, RwLock},
    time::{Duration, Instant},
};
use sysinfo::{Networks, System};
use tracing::{debug, instrument};

pub const METRICS_UPDATE_PERIOD: Duration = Duration::from_millis(100);

const SYSTEM_METRICS_REFRESH_PERIOD: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
struct InterfaceMetricHandles {
    interface: String,
    rx_bytes: Gauge,
    tx_bytes: Gauge,
}

/// Global singleton for the `Metrics` instance.
pub static METRICS: Lazy<Arc<Mutex<Option<Metrics>>>> = Lazy::new(|| Arc::new(Mutex::new(None)));

fn lock_metrics() -> MutexGuard<'static, Option<Metrics>> {
    METRICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Metrics struct to manage CPU, memory, and network metrics.
#[derive(Debug, Clone)]
pub struct Metrics {
    registry: Registry,
    common_labels: Arc<RwLock<Vec<(String, String)>>>, // Switched to RwLock for read-heavy workloads
    cpu_usage: Gauge,
    memory_usage: Gauge,
    network_metrics: Vec<InterfaceMetricHandles>,
    // Non-labelled custom gauges (name -> handle)
    custom_gauges: Arc<DashMap<String, IntGauge>>,
    // Labelled gauges (metric name -> GaugeVec)
    labelled_gauge_vecs: Arc<DashMap<String, IntGaugeVec>>,
    // Cache concrete labelled handles (name + label_values_key -> handle)
    labelled_handle_cache: Arc<DashMap<String, IntGauge>>,
    system: Arc<Mutex<System>>,
    last_system_refresh_at: Arc<Mutex<Option<Instant>>>,
    networks: Arc<Mutex<Networks>>,
}

pub struct MetricsBuilder {
    interfaces: Vec<String>,
    common_labels: Vec<(String, String)>,
    custom_gauges: HashMap<String, Opts>, // Custom gauges to be added
}

impl Default for MetricsBuilder {
    fn default() -> Self {
        Self::new()
    }
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
        self.common_labels
            .push((key.to_string(), value.to_string()));
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

        registry
            .register(Box::new(cpu_usage.clone()))
            .expect("Failed to register CPU usage gauge");
        registry
            .register(Box::new(memory_usage.clone()))
            .expect("Failed to register memory usage gauge");

        let mut network_metrics = Vec::new();
        for interface in self.interfaces {
            let sanitized_interface = Self::sanitize_name(&interface);
            let rx = Gauge::with_opts(Self::opts_with_labels(
                &format!("{sanitized_interface}_rx_bytes"),
                &format!("Received bytes for {interface}"),
                &self.common_labels,
            ))
            .expect("Failed to create RX gauge");
            let tx = Gauge::with_opts(Self::opts_with_labels(
                &format!("{sanitized_interface}_tx_bytes"),
                &format!("Transmitted bytes for {interface}"),
                &self.common_labels,
            ))
            .expect("Failed to create TX gauge");

            registry
                .register(Box::new(rx.clone()))
                .expect("Failed to register RX gauge");
            registry
                .register(Box::new(tx.clone()))
                .expect("Failed to register TX gauge");

            network_metrics.push(InterfaceMetricHandles {
                interface,
                rx_bytes: rx,
                tx_bytes: tx,
            });
        }

        let mut custom_gauges = HashMap::new();
        for (name, opts) in self.custom_gauges {
            let gauge = IntGauge::with_opts(opts).expect("Failed to create custom gauge");
            registry
                .register(Box::new(gauge.clone()))
                .expect("Failed to register custom gauge");
            custom_gauges.insert(name, gauge);
        }

        debug!("Metrics successfully built");

        let metrics = Metrics {
            registry,
            common_labels: Arc::new(RwLock::new(self.common_labels)),
            cpu_usage,
            memory_usage,
            network_metrics,
            custom_gauges: Arc::new(custom_gauges.into_iter().collect::<DashMap<_, _>>().into()),
            labelled_gauge_vecs: Arc::new(DashMap::new()),
            labelled_handle_cache: Arc::new(DashMap::new()),
            system: Arc::new(Mutex::new(System::new())),
            last_system_refresh_at: Arc::new(Mutex::new(None)),
            networks: Arc::new(Mutex::new(Networks::new_with_refreshed_list())),
        };

        let mut metrics_guard = lock_metrics();
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
    let metrics_guard = lock_metrics();
    if let Some(ref metrics) = *metrics_guard {
        return metrics.clone();
    }

    panic!("Metrics instance not initialized. Create a MetricsBuilder and call build().");
}

impl Metrics {
    /// Update metrics.
    #[instrument(skip_all)]
    pub fn update(&self) {
        let observed_at = Instant::now();

        let should_refresh_system_metrics = {
            let mut last_system_refresh_at = self
                .last_system_refresh_at
                .lock()
                .expect("Failed to lock system refresh state");
            let should_refresh = last_system_refresh_at.map_or(true, |last_refresh_at| {
                observed_at.saturating_duration_since(last_refresh_at)
                    >= SYSTEM_METRICS_REFRESH_PERIOD
            });
            if should_refresh {
                *last_system_refresh_at = Some(observed_at);
            }
            should_refresh
        };

        if should_refresh_system_metrics {
            let mut sys = self.system.lock().expect("Failed to lock system data");
            sys.refresh_all();

            let cpu_usage = (sys.global_cpu_usage() * 100.0).round() / 100.0;
            let memory_usage = sys.used_memory() as f64;

            self.cpu_usage.set(cpu_usage as f64);
            self.memory_usage.set(memory_usage);
        }

        if self.network_metrics.is_empty() {
            return;
        }

        let mut networks = self.networks.lock().expect("Failed to lock network data");
        networks.refresh(true);

        for handles in &self.network_metrics {
            if let Some(data) = networks.get(&handles.interface) {
                let rx_bytes = data.total_received();
                let tx_bytes = data.total_transmitted();
                handles.rx_bytes.set(rx_bytes as f64);
                handles.tx_bytes.set(tx_bytes as f64);
            }
        }
    }

    /// Create or get a Gauge (no labels).
    /// Switched to DashMap to avoid a coarse lock in hot paths.
    /// This implementation is race-free: only one thread creates & registers the collector.
    pub fn get_or_create_gauge(&self, name: &str, description: &str) -> Result<IntGauge, String> {
        // Fast path: most calls hit the cache.
        if let Some(entry) = self.custom_gauges.get(name) {
            return Ok(entry.value().clone());
        }
        // Slow path: atomically initialize if absent.
        // Using DashMap::entry ensures only one thread executes the "create & register" branch.
        // Capture labels outside entry to keep the critical section small.
        let labels = self
            .common_labels
            .read()
            .map_err(|_| "Failed to lock common labels".to_string())?;

        let vec_cloned = match self.custom_gauges.entry(name.to_string()) {
            Entry::Occupied(entry) => {
                // Lost the race; reuse the winner's vec
                entry.get().clone()
            }
            Entry::Vacant(entry) => {
                // Only one thread reaches here.
                let opts = MetricsBuilder::opts_with_labels(name, description, &labels);
                let gauge = IntGauge::with_opts(opts)
                    .map_err(|e| format!("Failed to create gauge: {e}"))?;
                // Register exactly once; any parallel attempts won't get here due to the entry lock.
                self.registry
                    .register(Box::new(gauge.clone()))
                    .map_err(|e| format!("Failed to register gauge: {e}"))?;
                entry.insert(gauge.clone());
                gauge
            }
        };

        Ok(vec_cloned)
    }

    /// Create or get a GaugeVec (labelled gauge family). Label keys must be stable for a given name.
    /// This implementation is race-free: only one thread creates & registers the collector.
    pub fn get_or_create_gauge_vec(
        &self,
        name: &str,
        description: &str,
        label_keys: &[&str],
    ) -> Result<IntGaugeVec, String> {
        // Fast path: most calls hit the cache.
        if let Some(entry) = self.labelled_gauge_vecs.get(name) {
            return Ok(entry.value().clone());
        }
        // Slow path: atomically initialize if absent.
        // Using DashMap::entry ensures only one thread executes the "create & register" branch.
        // Capture labels outside entry to keep the critical section small.
        let labels = self
            .common_labels
            .read()
            .map_err(|_| "Failed to lock common labels".to_string())?;

        let vec_cloned = match self.labelled_gauge_vecs.entry(name.to_string()) {
            Entry::Occupied(entry) => {
                // Lost the race; reuse the winner's vec
                entry.get().clone()
            }
            Entry::Vacant(entry) => {
                // Only one thread reaches here.
                let opts = MetricsBuilder::opts_with_labels(name, description, &labels);
                let vec_ = IntGaugeVec::new(opts, label_keys)
                    .map_err(|e| format!("Failed to create gauge vec: {e}"))?;
                // Register exactly once; any parallel attempts won't get here due to the entry lock.
                self.registry
                    .register(Box::new(vec_.clone()))
                    .map_err(|e| format!("Failed to register gauge vec: {e}"))?;
                entry.insert(vec_.clone());
                vec_
            }
        };

        Ok(vec_cloned)
    }

    /// Create or get a concrete labelled IntGauge handle (name + specific label values).
    /// Uses a fast DashMap cache keyed by name + '\x1F'-joined values.
    /// This implementation is race-free as it uses the race-free get_or_create_gauge_vec internally.
    pub fn get_or_create_labelled_gauge(
        &self,
        name: &str,
        description: &str,
        label_keys: &[&str],
        label_values: &[&str],
    ) -> Result<IntGauge, String> {
        debug_assert_eq!(label_keys.len(), label_values.len());
        let mut key = String::with_capacity(name.len() + 1 + 16 * label_values.len());
        key.push_str(name);
        key.push('|');
        for (i, v) in label_values.iter().enumerate() {
            if i > 0 {
                key.push('\x1F');
            }
            key.push_str(v);
        }
        if let Some(entry) = self.labelled_handle_cache.get(&key) {
            return Ok(entry.value().clone());
        }
        let vec_ = self.get_or_create_gauge_vec(name, description, label_keys)?;
        let handle = vec_
            .get_metric_with_label_values(label_values)
            .map_err(|e| format!("Failed to get labelled handle: {e}"))?;
        self.labelled_handle_cache.insert(key, handle.clone());
        Ok(handle)
    }

    /// Get the Prometheus registry.
    #[instrument(skip_all)]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

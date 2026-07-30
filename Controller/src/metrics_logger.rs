use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fs,
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use chrono::Utc;
use dashmap::{mapref::entry::Entry, DashMap};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{watch, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

const PROMETHEUS_URL: &str = "http://0.0.0.0:9090";
const PROMETHEUS_COLLECTION_PERIOD_MS: u64 = 1_000;
const WRITE_PERIOD_MS: u64 = 1_000;
const IN_MEMORY_WINDOW_MS: u64 = 60_000;
const PROMETHEUS_COLLECTION_PERIOD: Duration =
    Duration::from_millis(PROMETHEUS_COLLECTION_PERIOD_MS);
const WRITE_PERIOD: Duration = Duration::from_millis(WRITE_PERIOD_MS);
const METRIC_REFRESH_PERIOD: Duration = Duration::from_secs(5);
const IN_MEMORY_WINDOW_DURATION: Duration = Duration::from_millis(IN_MEMORY_WINDOW_MS);
const CATCHUP_FRACTION: f64 = 0.75;
const SOURCE_PROMETHEUS_PULL: &str = "prometheus_pull";
const SOURCE_AGENT_WEBSOCKET: &str = "agent_websocket";
const PROMETHEUS_CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
const PROMETHEUS_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const MIN_ROW_CAPACITY_TO_SHRINK: usize = 1024;
const ROW_CAPACITY_SHRINK_RATIO: usize = 4;
const MAX_EXPECTED_SAMPLE_RATE_HZ: usize = 120;
const IN_MEMORY_ROW_HEADROOM: usize = 2;
const PENDING_WRITE_PERIOD_HEADROOM: u64 = 8;
const PENDING_ROW_HEADROOM: usize = 2;
const ROW_CAPACITY_MARGIN: usize = 16;
const MAX_ROWS_PER_INSTANCE: usize = row_capacity_for_duration_ms(
    IN_MEMORY_WINDOW_MS,
    MAX_EXPECTED_SAMPLE_RATE_HZ,
    IN_MEMORY_ROW_HEADROOM,
);
const MAX_PENDING_ROWS_PER_INSTANCE: usize = row_capacity_for_duration_ms(
    WRITE_PERIOD_MS * PENDING_WRITE_PERIOD_HEADROOM,
    MAX_EXPECTED_SAMPLE_RATE_HZ,
    PENDING_ROW_HEADROOM,
);

const fn row_capacity_for_duration_ms(
    duration_ms: u64,
    sample_rate_hz: usize,
    headroom: usize,
) -> usize {
    let nominal_rows = ((duration_ms as usize * sample_rate_hz).saturating_add(999)) / 1_000;
    nominal_rows
        .saturating_mul(headroom)
        .saturating_add(ROW_CAPACITY_MARGIN)
}

fn skip_threshold(period: Duration) -> Duration {
    period.mul_f64(0.95)
}

fn in_memory_window_start_ms(latest_timestamp_ms: i64) -> i64 {
    let window_ms = i64::try_from(IN_MEMORY_WINDOW_DURATION.as_millis()).unwrap_or(i64::MAX);
    latest_timestamp_ms.saturating_sub(window_ms)
}

fn sanitize_label_value(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

fn is_identity_base_label(label: &str) -> bool {
    matches!(
        label,
        "__name__"
            | "instance"
            | "job"
            | "mode"
            | "stream_id"
            | "agent_node_id"
            | "agent_source_ip"
            | "agent_source_port"
            | "agent_source_instance"
    )
}

fn identity_extra_label_suffix<'a, I>(labels: I) -> String
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut extra_labels = labels
        .into_iter()
        .filter(|(key, _)| !is_identity_base_label(key))
        .map(|(key, value)| (sanitize_label_value(key), sanitize_label_value(value)))
        .collect::<Vec<_>>();
    extra_labels.sort_unstable();

    let mut suffix = String::new();
    for (key, value) in extra_labels {
        suffix.push_str("__");
        suffix.push_str(&key);
        suffix.push('_');
        suffix.push_str(&value);
    }

    suffix
}

fn identity_mode_suffix(mode: &str, stream_id: Option<&str>) -> String {
    if let Some(stream_id) = stream_id {
        format!(
            "{}_sid_{}",
            sanitize_label_value(mode),
            sanitize_label_value(stream_id)
        )
    } else {
        sanitize_label_value(mode)
    }
}

fn build_metric_identity(
    prefix: &str,
    instance: &str,
    mode: &str,
    stream_id: Option<&str>,
    extra_label_suffix: &str,
) -> String {
    format!(
        "{prefix}__{}_{}{}",
        sanitize_label_value(instance),
        identity_mode_suffix(mode, stream_id),
        extra_label_suffix,
    )
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum MetricsLoggerError {
    Reqwest(reqwest::Error),
    Io(std::io::Error),
    Serde(serde_json::Error),
    TaskJoin(tokio::task::JoinError),
    MissingData,
    AlreadyRunning,
    NotRunning,
    LoggerNotInitialized,
}

impl From<reqwest::Error> for MetricsLoggerError {
    fn from(err: reqwest::Error) -> Self {
        MetricsLoggerError::Reqwest(err)
    }
}

impl From<std::io::Error> for MetricsLoggerError {
    fn from(err: std::io::Error) -> Self {
        MetricsLoggerError::Io(err)
    }
}

impl From<serde_json::Error> for MetricsLoggerError {
    fn from(err: serde_json::Error) -> Self {
        MetricsLoggerError::Serde(err)
    }
}

impl From<tokio::task::JoinError> for MetricsLoggerError {
    fn from(err: tokio::task::JoinError) -> Self {
        MetricsLoggerError::TaskJoin(err)
    }
}

#[derive(Clone)]
pub struct MetricsLogger {
    folder_path: PathBuf,
    client: Client,
    buffers: Arc<DashMap<String, Arc<Mutex<MetricInstanceBuffer>>>>,
    dirty_instances: Arc<DashMap<String, u64>>,
    all_metrics: Arc<RwLock<BTreeSet<String>>>,
    prometheus_metrics: Arc<RwLock<Vec<String>>>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    seen_instances: Arc<DashMap<String, ()>>,
    accepting_agent_snapshots: Arc<AtomicBool>,
}

#[derive(Debug, Deserialize)]
pub struct AgentMetricsSnapshot {
    pub node_id: String,
    pub last_scan_completed_at_ms: Option<u64>,
    pub last_scan_duration_ms: Option<u64>,
    pub scan_rounds_completed: u64,
    pub targets: Vec<AgentTargetMetricsSnapshot>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct AgentTargetMetricsSnapshot {
    pub port: u16,
    pub source_ip: String,
    pub agent_node_id: String,
    pub source_instance: String,
    pub scraped_at_ms: Option<u64>,
    pub scrape_duration_ms: u64,
    pub scrape_ok: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub malformed_lines: usize,
    #[serde(default)]
    pub sample_count: usize,
    #[serde(default)]
    pub samples: Vec<AgentPrometheusSample>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentPrometheusSample {
    pub metric_name: String,
    pub labels: BTreeMap<String, String>,
    pub value: AgentSampleValue,
    pub timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AgentSampleValue {
    Float(f64),
    Text(String),
}

#[derive(Debug)]
struct NormalizedMetricRow {
    timestamp_ms: i64,
    source_kind: &'static str,
    identity: String,
    values: HashMap<String, f64>,
}

#[derive(Clone, Debug)]
struct MetricRowRecord {
    timestamp_ms: i64,
    source_kind: &'static str,
    values: HashMap<String, f64>,
}

#[derive(Clone, Debug)]
struct MetricInstanceBuffer {
    metric_names: BTreeSet<String>,
    rows: VecDeque<MetricRowRecord>,
    pending_rows: VecDeque<MetricRowRecord>,
    pending_rows_in_flight: usize,
    persisted_metric_names: Vec<String>,
    latest_timestamp_ms: Option<i64>,
    timestamps_are_ordered: bool,
    pending_overflow_warned: bool,
}

#[derive(Debug)]
struct MetricFlushSnapshot {
    metric_names: Vec<String>,
    previous_metric_names: Vec<String>,
    rows: Vec<MetricRowRecord>,
    pending_row_count: usize,
    rewrite_existing: bool,
}

struct CsvWriteJob {
    identity: String,
    revision: u64,
    path: PathBuf,
    snapshot: MetricFlushSnapshot,
}

impl MetricInstanceBuffer {
    fn append(&mut self, row: NormalizedMetricRow) {
        for metric_name in row.values.keys() {
            self.metric_names.insert(metric_name.clone());
        }

        let replaces_last_row = self.rows.back().is_some_and(|last| {
            last.timestamp_ms == row.timestamp_ms && last.source_kind == row.source_kind
        });
        if replaces_last_row {
            self.rows.pop_back();
        } else if self
            .rows
            .back()
            .is_some_and(|last| row.timestamp_ms < last.timestamp_ms)
        {
            self.timestamps_are_ordered = false;
        }

        let latest_timestamp_ms = self
            .latest_timestamp_ms
            .map_or(row.timestamp_ms, |latest| latest.max(row.timestamp_ms));
        self.latest_timestamp_ms = Some(latest_timestamp_ms);

        let record = MetricRowRecord {
            timestamp_ms: row.timestamp_ms,
            source_kind: row.source_kind,
            values: row.values,
        };
        if self.pending_rows.back().is_some_and(|last| {
            last.timestamp_ms == record.timestamp_ms && last.source_kind == record.source_kind
        }) && self.pending_rows.len() > self.pending_rows_in_flight
        {
            self.pending_rows.pop_back();
        }
        self.pending_rows.push_back(record.clone());
        self.trim_pending_rows_after_backlog();

        let cutoff_timestamp_ms = in_memory_window_start_ms(latest_timestamp_ms);
        if row.timestamp_ms < cutoff_timestamp_ms {
            self.trim_to_window(cutoff_timestamp_ms);
            return;
        }

        self.rows.push_back(record);
        self.trim_to_window(cutoff_timestamp_ms);
    }

    fn metric_names(&self) -> Vec<String> {
        self.metric_names.iter().cloned().collect()
    }

    fn flush_snapshot(&mut self) -> Option<MetricFlushSnapshot> {
        if self.pending_rows.is_empty() {
            return None;
        }

        self.pending_rows_in_flight = self.pending_rows.len();

        let mut metric_names = self.persisted_metric_names.clone();
        let mut existing_metrics = metric_names.iter().cloned().collect::<BTreeSet<_>>();
        for metric_name in &self.metric_names {
            if existing_metrics.insert(metric_name.clone()) {
                metric_names.push(metric_name.clone());
            }
        }

        Some(MetricFlushSnapshot {
            rewrite_existing: !self.persisted_metric_names.is_empty()
                && metric_names.len() > self.persisted_metric_names.len(),
            previous_metric_names: self.persisted_metric_names.clone(),
            rows: self.pending_rows.iter().cloned().collect(),
            pending_row_count: self.pending_rows.len(),
            metric_names,
        })
    }

    fn mark_flush_succeeded(
        &mut self,
        pending_row_count: usize,
        persisted_metric_names: Vec<String>,
    ) {
        for _ in 0..pending_row_count.min(self.pending_rows.len()) {
            self.pending_rows.pop_front();
        }
        self.pending_rows_in_flight = 0;
        self.persisted_metric_names = persisted_metric_names;
        if self.pending_rows.len() < MAX_PENDING_ROWS_PER_INSTANCE {
            self.pending_overflow_warned = false;
        }
    }

    fn last_n(&self, metric: &str, n: usize) -> Result<Vec<(i64, f64)>, MetricsLoggerError> {
        if !self.metric_names.contains(metric) {
            return Err(MetricsLoggerError::MissingData);
        }

        let len = self.rows.len();
        let start = len.saturating_sub(n);
        let mut out = Vec::with_capacity(n.min(len));
        for row in self.rows.iter().skip(start) {
            out.push((
                row.timestamp_ms,
                row.values.get(metric).copied().unwrap_or(f64::NAN),
            ));
        }
        Ok(out)
    }

    fn window_ms(
        &self,
        metric: &str,
        window_ms: i64,
    ) -> Result<Vec<(i64, f64)>, MetricsLoggerError> {
        if !self.metric_names.contains(metric) {
            return Err(MetricsLoggerError::MissingData);
        }

        let latest_timestamp_ms = self
            .rows
            .iter()
            .map(|row| row.timestamp_ms)
            .max()
            .ok_or(MetricsLoggerError::MissingData)?;
        let cutoff_timestamp_ms = latest_timestamp_ms.saturating_sub(window_ms.max(0));

        let out = self
            .rows
            .iter()
            .filter(|row| row.timestamp_ms >= cutoff_timestamp_ms)
            .map(|row| {
                (
                    row.timestamp_ms,
                    row.values.get(metric).copied().unwrap_or(f64::NAN),
                )
            })
            .collect::<Vec<_>>();

        if out.is_empty() {
            return Err(MetricsLoggerError::MissingData);
        }

        Ok(out)
    }

    fn trim_to_window(&mut self, cutoff_timestamp_ms: i64) {
        if self.timestamps_are_ordered {
            while self
                .rows
                .front()
                .is_some_and(|row| row.timestamp_ms < cutoff_timestamp_ms)
            {
                self.rows.pop_front();
            }
        } else {
            self.rows
                .retain(|row| row.timestamp_ms >= cutoff_timestamp_ms);
            self.timestamps_are_ordered = timestamps_are_ordered(&self.rows);
        }

        while self.rows.len() > MAX_ROWS_PER_INSTANCE {
            self.rows.pop_front();
        }

        self.shrink_after_burst();
    }

    fn shrink_after_burst(&mut self) {
        let len = self.rows.len().max(1);
        if self.rows.capacity() >= MIN_ROW_CAPACITY_TO_SHRINK
            && self.rows.capacity() > len.saturating_mul(ROW_CAPACITY_SHRINK_RATIO)
        {
            self.rows.shrink_to_fit();
        }
    }

    fn trim_pending_rows_after_backlog(&mut self) {
        if self.pending_rows.len() <= MAX_PENDING_ROWS_PER_INSTANCE {
            return;
        }

        while self.pending_rows.len() > MAX_PENDING_ROWS_PER_INSTANCE {
            self.pending_rows.pop_front();
            self.pending_rows_in_flight = self.pending_rows_in_flight.saturating_sub(1);
        }

        if !self.pending_overflow_warned {
            warn!(
                "[metrics_logger] Pending metrics rows exceeded {}; dropping oldest unflushed rows for this instance",
                MAX_PENDING_ROWS_PER_INSTANCE
            );
            self.pending_overflow_warned = true;
        }
    }
}

impl Default for MetricInstanceBuffer {
    fn default() -> Self {
        Self {
            metric_names: BTreeSet::new(),
            rows: VecDeque::new(),
            pending_rows: VecDeque::new(),
            pending_rows_in_flight: 0,
            persisted_metric_names: Vec::new(),
            latest_timestamp_ms: None,
            timestamps_are_ordered: true,
            pending_overflow_warned: false,
        }
    }
}

fn timestamps_are_ordered(rows: &VecDeque<MetricRowRecord>) -> bool {
    rows.iter()
        .map(|row| row.timestamp_ms)
        .try_fold(i64::MIN, |previous, current| {
            if current < previous {
                Err(())
            } else {
                Ok(current)
            }
        })
        .is_ok()
}

impl MetricsLogger {
    pub async fn new(experiment_name: &str) -> Result<Self, MetricsLoggerError> {
        let path = PathBuf::from(format!("./dist/experiments/{experiment_name}"));
        if !path.exists() {
            fs::create_dir_all(&path)?;
        }
        let contents = std::fs::read_to_string(&path)?;

        let start_time = Utc::now().timestamp_millis();
        let sanitized_name = experiment_name.replace('.', "_");
        let folder_path: PathBuf = [
            "metrics",
            "measurements",
            &sanitized_name,
            &start_time.to_string(),
        ]
        .iter()
        .collect();
        fs::create_dir_all(&folder_path)?;

        // Write a copy of the experiment file
        let experiment_file_path = folder_path.join(format!("experiment_{start_time}.yaml"));
        let mut file = fs::File::create(&experiment_file_path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;

        let client = Client::builder()
            .connect_timeout(PROMETHEUS_CONNECT_TIMEOUT)
            .timeout(PROMETHEUS_REQUEST_TIMEOUT)
            .build()?;
        let metrics = Self::fetch_all_metrics(&client).await?;

        info!("[metrics_logger] Saving CSV files to {:?}", folder_path);
        info!("[metrics_logger] Found {} metrics.", metrics.len());

        Ok(Self {
            folder_path,
            client,
            buffers: Arc::new(DashMap::new()),
            dirty_instances: Arc::new(DashMap::new()),
            all_metrics: Arc::new(RwLock::new(metrics.iter().cloned().collect())),
            prometheus_metrics: Arc::new(RwLock::new(metrics)),
            shutdown_tx: Arc::new(Mutex::new(None)),
            task_handles: Arc::new(Mutex::new(Vec::new())),
            seen_instances: Arc::new(DashMap::new()),
            accepting_agent_snapshots: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn fetch_all_metrics(client: &Client) -> Result<Vec<String>, MetricsLoggerError> {
        let url = format!("{PROMETHEUS_URL}/api/v1/label/__name__/values");
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_connect() {
                    return Ok(Vec::new());
                } else {
                    return Err(MetricsLoggerError::Reqwest(e));
                }
            }
        };
        let json: Value = resp.json().await?;
        let mut metrics: Vec<String> = json["data"]
            .as_array()
            .ok_or(MetricsLoggerError::MissingData)?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        metrics.sort();
        Ok(metrics)
    }

    async fn ensure_known_metrics<I>(&self, metric_names: I)
    where
        I: IntoIterator<Item = String>,
    {
        let metric_names = metric_names.into_iter().collect::<Vec<_>>();
        if metric_names.is_empty() {
            return;
        }

        let unknown_metrics = {
            let guard = self.all_metrics.read().await;
            metric_names
                .into_iter()
                .filter(|metric_name| !guard.contains(metric_name))
                .collect::<Vec<_>>()
        };

        if unknown_metrics.is_empty() {
            return;
        }

        let mut guard = self.all_metrics.write().await;
        for metric_name in unknown_metrics {
            if guard.insert(metric_name.clone()) {
                info!("[metrics_logger] Discovered new metric: {}", metric_name);
            }
        }
    }

    async fn refresh_metrics(&self) -> Result<(), MetricsLoggerError> {
        let metrics = Self::fetch_all_metrics(&self.client).await?;
        {
            let mut guard = self.prometheus_metrics.write().await;
            *guard = metrics.clone();
        }
        self.ensure_known_metrics(metrics).await;
        Ok(())
    }

    async fn query_metric(&self, metric_name: &str) -> Result<Vec<Value>, MetricsLoggerError> {
        let url: String = format!("{PROMETHEUS_URL}/api/v1/query");
        let resp = self
            .client
            .get(url)
            .query(&[("query", metric_name)])
            .send()
            .await?;
        let json: Value = resp.json().await?;
        Ok(json["data"]["result"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    // Batch query: fetch multiple metrics at once via regex on __name__
    async fn query_metrics_batch(
        &self,
        metric_names: &[String],
    ) -> Result<Vec<Value>, MetricsLoggerError> {
        if metric_names.is_empty() {
            return Ok(Vec::new());
        }
        // Chunk defensively to avoid very long query strings
        const CHUNK: usize = 200;
        let mut out = Vec::new();
        for chunk in metric_names.chunks(CHUNK) {
            // NOTE: metric names in Prometheus are [a-zA-Z_:][a-zA-Z0-9_:]*
            // they don’t need regex escaping here; if we have exotic names, escape as needed.
            let pat = chunk.join("|");
            let q = format!("{{__name__=~\"{pat}\"}}");
            let arr = self.query_metric(&q).await?;
            out.extend(arr);
        }
        Ok(out)
    }

    pub async fn start(&self) -> Result<(), MetricsLoggerError> {
        let (tx, rx) = watch::channel(false);
        {
            let mut shutdown_guard = self.shutdown_tx.lock().await;
            if shutdown_guard.is_some() {
                return Err(MetricsLoggerError::AlreadyRunning);
            }
            *shutdown_guard = Some(tx);
            self.accepting_agent_snapshots
                .store(true, Ordering::Release);
        }

        let collector_logger = self.clone();
        let collector_rx = rx.clone();
        let collector_handle = tokio::spawn(async move {
            collector_logger
                .run_prometheus_collection_loop(collector_rx)
                .await;
        });

        let writer_logger = self.clone();
        let writer_handle = tokio::spawn(async move {
            writer_logger.run_writer_loop(rx).await;
        });

        let mut task_guard = self.task_handles.lock().await;
        *task_guard = vec![collector_handle, writer_handle];
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), MetricsLoggerError> {
        self.accepting_agent_snapshots
            .store(false, Ordering::Release);

        let tx = {
            let mut shutdown_guard = self.shutdown_tx.lock().await;
            shutdown_guard
                .take()
                .ok_or(MetricsLoggerError::NotRunning)?
        };
        let _ = tx.send(true);

        let handles = {
            let mut task_guard = self.task_handles.lock().await;
            std::mem::take(&mut *task_guard)
        };

        for handle in handles {
            let _ = handle.await;
        }

        self.flush_dirty_buffers().await?;
        Ok(())
    }

    async fn run_prometheus_collection_loop(&self, rx: watch::Receiver<bool>) {
        let start = tokio::time::Instant::now();
        let mut tick_idx: u64 = 1;
        let mut last_metrics_refresh_at = start - METRIC_REFRESH_PERIOD;

        loop {
            if *rx.borrow() {
                break;
            }

            let now = tokio::time::Instant::now();
            if now.duration_since(last_metrics_refresh_at) >= METRIC_REFRESH_PERIOD {
                if let Err(e) = self.refresh_metrics().await {
                    error!("[metrics_logger] Error refreshing metrics: {:?}", e);
                }
                last_metrics_refresh_at = tokio::time::Instant::now();
            }

            match self.collect_prometheus_rows().await {
                Ok(rows) => {
                    if let Err(e) = self.append_normalized_rows(rows).await {
                        error!("[metrics_logger] Error appending Prometheus rows: {:?}", e);
                    }
                }
                Err(e) => error!("[metrics_logger] Error collecting Prometheus rows: {:?}", e),
            }

            wait_for_next_tick(start, &mut tick_idx, PROMETHEUS_COLLECTION_PERIOD).await;
        }
    }

    async fn run_writer_loop(&self, rx: watch::Receiver<bool>) {
        let start = tokio::time::Instant::now();
        let mut tick_idx: u64 = 1;

        loop {
            if *rx.borrow() {
                break;
            }

            if let Err(e) = self.flush_dirty_buffers().await {
                error!("[metrics_logger] Error writing metrics to disk: {:?}", e);
            }

            wait_for_next_tick(start, &mut tick_idx, WRITE_PERIOD).await;
        }
    }

    async fn collect_prometheus_rows(
        &self,
    ) -> Result<Vec<NormalizedMetricRow>, MetricsLoggerError> {
        let metrics_list = { self.prometheus_metrics.read().await.clone() };
        if metrics_list.is_empty() {
            return Ok(Vec::new());
        }
        let results = match self.query_metrics_batch(&metrics_list).await {
            Ok(results) => results,
            Err(err) if is_prometheus_unavailable_error(&err) => {
                let mut guard = self.prometheus_metrics.write().await;
                guard.clear();
                return Ok(Vec::new());
            }
            Err(err) => return Err(err),
        };

        let mut rows_by_identity: HashMap<String, NormalizedMetricRow> = HashMap::new();
        let mut observed_metrics = BTreeSet::new();

        for res in results {
            let metric_obj = &res["metric"];
            // instant vector: "value" = [timestamp, "val"]
            let value_arr = &res["value"];
            let sample_timestamp_ms = prometheus_value_timestamp_ms(value_arr)
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let metric_name = metric_obj
                .get("__name__")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let identity = prometheus_identity_from_metric(metric_obj);
            let value = value_arr
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("0.0")
                .parse::<f64>()
                .unwrap_or(f64::NAN);

            observed_metrics.insert(metric_name.clone());
            rows_by_identity
                .entry(identity.clone())
                .and_modify(|row| {
                    row.timestamp_ms = row.timestamp_ms.max(sample_timestamp_ms);
                })
                .or_insert_with(|| NormalizedMetricRow {
                    timestamp_ms: sample_timestamp_ms,
                    source_kind: SOURCE_PROMETHEUS_PULL,
                    identity,
                    values: HashMap::new(),
                })
                .values
                .insert(metric_name, value);
        }

        self.ensure_known_metrics(observed_metrics.into_iter().collect::<Vec<_>>())
            .await;
        Ok(rows_by_identity.into_values().collect())
    }

    pub async fn ingest_agent_snapshot(
        &self,
        snapshot: AgentMetricsSnapshot,
    ) -> Result<(), MetricsLoggerError> {
        if !self.accepting_agent_snapshots.load(Ordering::Acquire) {
            return Err(MetricsLoggerError::NotRunning);
        }

        let _controller_round = snapshot.scan_rounds_completed;
        let _controller_scan_done = snapshot.last_scan_completed_at_ms;
        let _controller_scan_duration = snapshot.last_scan_duration_ms;
        let _controller_node_id = &snapshot.node_id;

        let mut rows_by_identity: HashMap<String, NormalizedMetricRow> = HashMap::new();
        let mut observed_metrics = BTreeSet::new();

        for target in snapshot.targets {
            let _ = (
                &target.error,
                target.malformed_lines,
                target.sample_count,
                target.scrape_duration_ms,
            );
            if !target.scrape_ok {
                continue;
            }

            let row_timestamp = target
                .scraped_at_ms
                .and_then(|ts| i64::try_from(ts).ok())
                .unwrap_or_else(|| Utc::now().timestamp_millis());

            let target_clone = target.clone();
            for sample in target.samples {
                let metric_name = sample.metric_name;
                let identity = agent_identity_from_parts(&target_clone, &sample.labels);
                let value = sample_value_to_f64(&sample.value);
                let sample_timestamp = sample.timestamp_ms.unwrap_or(row_timestamp);

                observed_metrics.insert(metric_name.clone());
                rows_by_identity
                    .entry(identity.clone())
                    .or_insert_with(|| NormalizedMetricRow {
                        timestamp_ms: sample_timestamp,
                        source_kind: SOURCE_AGENT_WEBSOCKET,
                        identity,
                        values: HashMap::new(),
                    })
                    .values
                    .insert(metric_name, value);
            }
        }

        if rows_by_identity.is_empty() {
            return Ok(());
        }

        self.ensure_known_metrics(observed_metrics.into_iter().collect::<Vec<_>>())
            .await;
        self.append_normalized_rows(rows_by_identity.into_values().collect())
            .await
    }

    pub async fn list_metric_names(&self) -> Vec<String> {
        self.all_metrics.read().await.iter().cloned().collect()
    }

    pub fn list_instances(&self) -> Vec<String> {
        let mut instances = self
            .buffers
            .iter()
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        instances.sort();
        instances.dedup();
        instances
    }

    pub async fn list_metrics_for_instance(
        &self,
        instance: &str,
    ) -> Result<Vec<String>, MetricsLoggerError> {
        let buffer_lock = self
            .buffers
            .get(instance)
            .ok_or(MetricsLoggerError::MissingData)?
            .value()
            .clone();
        let metrics = buffer_lock.lock().await.metric_names();
        Ok(metrics)
    }

    pub async fn query_metric_series(
        &self,
        metric: &str,
        instance: Option<&str>,
        n: usize,
    ) -> Result<Vec<(String, Vec<(i64, f64)>)>, MetricsLoggerError> {
        let instances = if let Some(instance) = instance {
            vec![instance.to_string()]
        } else {
            self.list_instances()
        };

        let mut out = Vec::new();
        for instance_name in instances {
            match self.get_last_n(&instance_name, metric, n).await {
                Ok(values) => out.push((instance_name, values)),
                Err(MetricsLoggerError::MissingData) => {}
                Err(err) => return Err(err),
            }
        }

        Ok(out)
    }

    fn get_or_create_buffer(
        &self,
        identity: &str,
        source_kind: &'static str,
    ) -> Arc<Mutex<MetricInstanceBuffer>> {
        match self.buffers.entry(identity.to_string()) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                info!(
                    "[metrics_logger] Discovered new metrics instance: {} (source={})",
                    identity, source_kind
                );
                let buffer = Arc::new(Mutex::new(MetricInstanceBuffer::default()));
                entry.insert(buffer.clone());
                self.seen_instances.insert(identity.to_string(), ());
                buffer
            }
        }
    }

    async fn append_normalized_rows(
        &self,
        rows: Vec<NormalizedMetricRow>,
    ) -> Result<(), MetricsLoggerError> {
        if rows.is_empty() {
            return Ok(());
        }

        for row in rows {
            let identity = row.identity.clone();
            let buffer_lock = self.get_or_create_buffer(&identity, row.source_kind);
            buffer_lock.lock().await.append(row);
            mark_identity_dirty(&self.dirty_instances, &identity);
        }

        Ok(())
    }

    async fn flush_dirty_buffers(&self) -> Result<(), MetricsLoggerError> {
        let dirty_instances = self
            .dirty_instances
            .iter()
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect::<Vec<_>>();

        if dirty_instances.is_empty() {
            return Ok(());
        }

        let mut write_jobs = Vec::with_capacity(dirty_instances.len());
        for (identity, revision) in dirty_instances {
            let Some(buffer_lock) = self
                .buffers
                .get(&identity)
                .map(|entry| entry.value().clone())
            else {
                self.dirty_instances.remove(&identity);
                continue;
            };

            let csv_path = self
                .folder_path
                .join(format!("metrics_{}.csv", identity.replace(':', "_")));
            let snapshot = {
                let mut buffer = buffer_lock.lock().await;
                buffer.flush_snapshot()
            };
            let Some(snapshot) = snapshot else {
                self.dirty_instances.remove(&identity);
                continue;
            };
            write_jobs.push(CsvWriteJob {
                identity,
                revision,
                path: csv_path,
                snapshot,
            });
        }

        let completed_revisions = write_jobs
            .iter()
            .map(|job| {
                (
                    job.identity.clone(),
                    job.revision,
                    job.snapshot.pending_row_count,
                    job.snapshot.metric_names.clone(),
                )
            })
            .collect::<Vec<_>>();

        if write_jobs.is_empty() {
            return Ok(());
        }

        tokio::task::spawn_blocking(move || -> Result<(), MetricsLoggerError> {
            for job in write_jobs {
                debug!(
                    "[metrics_logger] Writing metrics for instance: {}",
                    job.identity
                );
                write_metric_rows_csv(&job.path, &job.snapshot)?;
            }
            Ok(())
        })
        .await??;

        for (identity, revision, pending_row_count, metric_names) in completed_revisions {
            if let Some(buffer_lock) = self
                .buffers
                .get(&identity)
                .map(|entry| entry.value().clone())
            {
                buffer_lock
                    .lock()
                    .await
                    .mark_flush_succeeded(pending_row_count, metric_names);
            }

            let should_clear_dirty = self
                .dirty_instances
                .get(&identity)
                .map(|entry| *entry.value() == revision)
                .unwrap_or(false);
            if should_clear_dirty {
                self.dirty_instances.remove(&identity);
            }
        }

        Ok(())
    }

    pub async fn get_last_n(
        &self,
        instance: &str,
        metric: &str,
        n: usize,
    ) -> Result<Vec<(i64, f64)>, MetricsLoggerError> {
        let buffer_lock = self
            .buffers
            .get(instance)
            .ok_or(MetricsLoggerError::MissingData)?
            .value()
            .clone();
        let values = buffer_lock.lock().await.last_n(metric, n);
        values
    }

    pub async fn get_window_ms(
        &self,
        instance: &str,
        metric: &str,
        window_ms: i64,
    ) -> Result<Vec<(i64, f64)>, MetricsLoggerError> {
        let buffer_lock = self
            .buffers
            .get(instance)
            .ok_or(MetricsLoggerError::MissingData)?
            .value()
            .clone();
        let values = buffer_lock.lock().await.window_ms(metric, window_ms);
        values
    }
}

async fn wait_for_next_tick(start: tokio::time::Instant, tick_idx: &mut u64, period: Duration) {
    let now = tokio::time::Instant::now();
    let target = start + period.saturating_mul(*tick_idx as u32);

    if now < target {
        tokio::time::sleep_until(target).await;
        *tick_idx = tick_idx.saturating_add(1);
        return;
    }

    let lateness = now.saturating_duration_since(target);
    let skip_threshold = skip_threshold(period);
    if lateness < skip_threshold {
        let catchup_cap = period.mul_f64(CATCHUP_FRACTION);
        let shave = if lateness > catchup_cap {
            catchup_cap
        } else {
            lateness
        };
        let sleep_dur = period.saturating_sub(shave);
        if !sleep_dur.is_zero() {
            sleep(sleep_dur).await;
        }
        *tick_idx = tick_idx.saturating_add(1);
        return;
    }

    let elapsed = now - start;
    let full_ticks = (elapsed.as_nanos() / period.as_nanos()) as u64;
    *tick_idx = full_ticks + 1;
}

fn is_prometheus_unavailable_error(err: &MetricsLoggerError) -> bool {
    matches!(err, MetricsLoggerError::Reqwest(reqwest_err) if reqwest_err.is_connect() || reqwest_err.is_timeout())
}

fn prometheus_value_timestamp_ms(value_arr: &Value) -> Option<i64> {
    let timestamp_seconds = value_arr.get(0)?.as_f64()?;
    Some((timestamp_seconds * 1000.0).round() as i64)
}

fn mark_identity_dirty(dirty_instances: &DashMap<String, u64>, identity: &str) {
    dirty_instances
        .entry(identity.to_string())
        .and_modify(|revision| *revision = revision.saturating_add(1))
        .or_insert(1);
}

fn write_metric_rows_csv(
    csv_path: &PathBuf,
    snapshot: &MetricFlushSnapshot,
) -> Result<(), MetricsLoggerError> {
    if snapshot.rows.is_empty() {
        return Ok(());
    }

    if snapshot.previous_metric_names.is_empty() {
        return create_metric_csv(csv_path, snapshot);
    }

    if snapshot.rewrite_existing {
        return rewrite_metric_csv_with_expanded_schema(csv_path, snapshot);
    }

    let file_exists = csv_path.exists();
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(csv_path)?;
    let mut writer = io::BufWriter::new(file);

    if !file_exists {
        write_csv_header(&mut writer, &snapshot.metric_names)?;
    }
    write_metric_rows(&mut writer, &snapshot.metric_names, &snapshot.rows)?;
    writer.flush()?;
    Ok(())
}

fn create_metric_csv(
    csv_path: &PathBuf,
    snapshot: &MetricFlushSnapshot,
) -> Result<(), MetricsLoggerError> {
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(csv_path)?;
    let mut writer = io::BufWriter::new(file);

    write_csv_header(&mut writer, &snapshot.metric_names)?;
    write_metric_rows(&mut writer, &snapshot.metric_names, &snapshot.rows)?;
    writer.flush()?;
    Ok(())
}

fn rewrite_metric_csv_with_expanded_schema(
    csv_path: &PathBuf,
    snapshot: &MetricFlushSnapshot,
) -> Result<(), MetricsLoggerError> {
    let temp_path = csv_path.with_extension("csv.tmp");
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temp_path)?;
    let mut writer = io::BufWriter::new(file);
    write_csv_header(&mut writer, &snapshot.metric_names)?;

    if csv_path.exists() {
        let old_file = fs::File::open(csv_path)?;
        let reader = BufReader::new(old_file);
        let added_metric_count = snapshot
            .metric_names
            .len()
            .saturating_sub(snapshot.previous_metric_names.len());

        for (line_index, line) in reader.lines().enumerate() {
            let line = line?;
            if line_index == 0 || line.trim().is_empty() {
                continue;
            }

            writer.write_all(line.as_bytes())?;
            for _ in 0..added_metric_count {
                writer.write_all(b",NaN")?;
            }
            writer.write_all(b"\n")?;
        }
    }

    write_metric_rows(&mut writer, &snapshot.metric_names, &snapshot.rows)?;
    writer.flush()?;
    fs::rename(temp_path, csv_path)?;
    Ok(())
}

fn write_csv_header<W: Write>(writer: &mut W, metric_names: &[String]) -> io::Result<()> {
    write_csv_cell(writer, "timestamp")?;
    writer.write_all(b",")?;
    write_csv_cell(writer, "source_kind")?;
    for metric_name in metric_names {
        writer.write_all(b",")?;
        write_csv_cell(writer, metric_name)?;
    }
    writer.write_all(b"\n")
}

fn write_metric_rows<W: Write>(
    writer: &mut W,
    metric_names: &[String],
    rows: &[MetricRowRecord],
) -> io::Result<()> {
    for row in rows {
        write!(writer, "{}", row.timestamp_ms)?;
        writer.write_all(b",")?;
        write_csv_cell(writer, row.source_kind)?;
        for metric_name in metric_names {
            writer.write_all(b",")?;
            let value = row.values.get(metric_name).copied().unwrap_or(f64::NAN);
            write!(writer, "{value}")?;
        }
        writer.write_all(b"\n")?;
    }

    Ok(())
}

fn write_csv_cell<W: Write>(writer: &mut W, value: &str) -> io::Result<()> {
    let needs_quotes = value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'));

    if !needs_quotes {
        writer.write_all(value.as_bytes())?;
        return Ok(());
    }

    writer.write_all(b"\"")?;
    for byte in value.bytes() {
        if byte == b'"' {
            writer.write_all(b"\"\"")?;
        } else {
            writer.write_all(&[byte])?;
        }
    }
    writer.write_all(b"\"")
}

fn prometheus_identity_from_metric(metric_obj: &Value) -> String {
    let instance = metric_obj
        .get("instance")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mode = metric_obj
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let stream_id = metric_obj.get("stream_id").and_then(Value::as_str);
    let extra_label_suffix = metric_obj
        .as_object()
        .map(|labels| {
            identity_extra_label_suffix(
                labels
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|value| (key.as_str(), value))),
            )
        })
        .unwrap_or_default();

    build_metric_identity("prom", instance, mode, stream_id, &extra_label_suffix)
}

fn agent_identity_from_parts(
    target: &AgentTargetMetricsSnapshot,
    labels: &BTreeMap<String, String>,
) -> String {
    let source_instance = labels
        .get("agent_source_instance")
        .map(String::as_str)
        .or_else(|| labels.get("instance").map(String::as_str))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if !target.source_instance.trim().is_empty() {
                target.source_instance.as_str()
            } else {
                "unknown"
            }
        });

    let mode = labels.get("mode").map(String::as_str).unwrap_or("unknown");
    let stream_id = labels.get("stream_id").map(String::as_str);
    let extra_label_suffix = identity_extra_label_suffix(
        labels
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );

    build_metric_identity(
        "agent",
        source_instance,
        mode,
        stream_id,
        &extra_label_suffix,
    )
}

fn sample_value_to_f64(value: &AgentSampleValue) -> f64 {
    match value {
        AgentSampleValue::Float(v) => *v,
        AgentSampleValue::Text(text) => match text.as_str() {
            "Inf" | "+Inf" => f64::INFINITY,
            "-Inf" => f64::NEG_INFINITY,
            "NaN" => f64::NAN,
            other => other.parse::<f64>().unwrap_or(f64::NAN),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_row(timestamp_ms: i64) -> NormalizedMetricRow {
        NormalizedMetricRow {
            timestamp_ms,
            source_kind: SOURCE_AGENT_WEBSOCKET,
            identity: "agent__test_client".to_string(),
            values: HashMap::from([("metric_a".to_string(), timestamp_ms as f64)]),
        }
    }

    fn test_record(timestamp_ms: i64, values: HashMap<String, f64>) -> MetricRowRecord {
        MetricRowRecord {
            timestamp_ms,
            source_kind: SOURCE_AGENT_WEBSOCKET,
            values,
        }
    }

    fn temp_metric_csv_path(test_name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "metrics_logger_{test_name}_{}_{}.csv",
            std::process::id(),
            suffix
        ))
    }

    #[test]
    fn row_caps_are_derived_from_window_and_write_period() {
        assert_eq!(MAX_ROWS_PER_INSTANCE, 14_416);
        assert_eq!(MAX_PENDING_ROWS_PER_INSTANCE, 1_936);
        assert!(MAX_PENDING_ROWS_PER_INSTANCE < MAX_ROWS_PER_INSTANCE);
    }

    #[test]
    fn prometheus_identity_preserves_existing_layout_without_extra_labels() {
        let metric = json!({
            "__name__": "freeze_events_total",
            "instance": "10.0.0.1:9090",
            "job": "dynamic_nodes",
            "mode": "receiver",
            "stream_id": "7",
        });

        assert_eq!(
            prometheus_identity_from_metric(&metric),
            "prom__10_0_0_1_9090_receiver_sid_7"
        );
    }

    #[test]
    fn prometheus_identity_splits_additional_labels_deterministically() {
        let metric = json!({
            "__name__": "cache_bytes_served_total",
            "instance": "10.0.0.1:9090",
            "mode": "proxy",
            "tier": "disk",
            "transport": "flute",
        });

        assert_eq!(
            prometheus_identity_from_metric(&metric),
            "prom__10_0_0_1_9090_proxy__tier_disk__transport_flute"
        );
    }

    #[test]
    fn agent_identity_splits_additional_labels_without_changing_base_fields() {
        let target = AgentTargetMetricsSnapshot {
            port: 3380,
            source_ip: "11.0.2.2".to_string(),
            agent_node_id: "node-a".to_string(),
            source_instance: "11.0.2.2:3380".to_string(),
            scraped_at_ms: None,
            scrape_duration_ms: 0,
            scrape_ok: true,
            error: None,
            malformed_lines: 0,
            sample_count: 0,
            samples: Vec::new(),
        };
        let labels = BTreeMap::from([
            ("agent_node_id".to_string(), "node-a".to_string()),
            (
                "agent_source_instance".to_string(),
                "11.0.2.2:3380".to_string(),
            ),
            ("agent_source_ip".to_string(), "11.0.2.2".to_string()),
            ("agent_source_port".to_string(), "3380".to_string()),
            ("mode".to_string(), "proxy".to_string()),
            ("stream_id".to_string(), "camera-a".to_string()),
            ("tier".to_string(), "mem".to_string()),
        ]);

        assert_eq!(
            agent_identity_from_parts(&target, &labels),
            "agent__11_0_2_2_3380_proxy_sid_camera_a__tier_mem"
        );
    }

    #[test]
    fn metric_buffer_pops_front_for_ordered_window_eviction() {
        let mut buffer = MetricInstanceBuffer::default();

        for tick in 0..=350 {
            buffer.append(test_row(tick * 200));
        }

        assert_eq!(buffer.rows.front().unwrap().timestamp_ms, 10_000);
        assert_eq!(buffer.rows.back().unwrap().timestamp_ms, 70_000);
        assert_eq!(buffer.rows.len(), 301);
        assert_eq!(
            buffer.last_n("metric_a", 1).unwrap(),
            vec![(70_000, 70_000.0)]
        );
    }

    #[test]
    fn metric_buffer_full_trims_when_timestamps_arrive_out_of_order() {
        let mut buffer = MetricInstanceBuffer::default();

        buffer.append(test_row(10_000));
        buffer.append(test_row(20_000));
        buffer.append(test_row(15_000));
        buffer.append(test_row(80_000));

        let retained_timestamps = buffer
            .rows
            .iter()
            .map(|row| row.timestamp_ms)
            .collect::<Vec<_>>();
        assert_eq!(retained_timestamps, vec![20_000, 80_000]);
        assert!(buffer.timestamps_are_ordered);
    }

    #[test]
    fn metric_buffer_caps_rows_when_samples_burst_within_window() {
        let mut buffer = MetricInstanceBuffer::default();

        for timestamp_ms in 0..(MAX_ROWS_PER_INSTANCE + 50) {
            buffer.append(test_row(timestamp_ms as i64));
        }

        assert_eq!(buffer.rows.len(), MAX_ROWS_PER_INSTANCE);
        assert_eq!(
            buffer.rows.back().unwrap().timestamp_ms,
            (MAX_ROWS_PER_INSTANCE + 49) as i64
        );
    }

    #[test]
    fn metric_buffer_keeps_evicted_rows_pending_until_flush_succeeds() {
        let mut buffer = MetricInstanceBuffer::default();

        for tick in 0..=350 {
            buffer.append(test_row(tick * 200));
        }

        assert_eq!(buffer.rows.len(), 301);
        assert_eq!(buffer.pending_rows.len(), 351);

        let snapshot = buffer.flush_snapshot().unwrap();
        assert_eq!(snapshot.rows.len(), 351);
        buffer.mark_flush_succeeded(snapshot.pending_row_count, snapshot.metric_names);

        assert!(buffer.pending_rows.is_empty());
        assert_eq!(buffer.rows.len(), 301);
    }

    #[test]
    fn metric_buffer_coalesces_pending_duplicate_until_flush_starts() {
        let mut buffer = MetricInstanceBuffer::default();

        buffer.append(test_row(1_000));
        buffer.append(test_row(1_000));
        assert_eq!(buffer.pending_rows.len(), 1);

        let snapshot = buffer.flush_snapshot().unwrap();
        assert_eq!(snapshot.rows.len(), 1);

        buffer.append(test_row(1_000));
        assert_eq!(buffer.pending_rows.len(), 2);
    }

    #[test]
    fn metric_csv_appends_pending_rows_without_rewriting_existing_rows() {
        let csv_path = temp_metric_csv_path("append");
        let first = MetricFlushSnapshot {
            metric_names: vec!["metric_a".to_string()],
            previous_metric_names: Vec::new(),
            rows: vec![test_record(
                1_000,
                HashMap::from([("metric_a".to_string(), 1.0)]),
            )],
            pending_row_count: 1,
            rewrite_existing: false,
        };
        let second = MetricFlushSnapshot {
            metric_names: vec!["metric_a".to_string()],
            previous_metric_names: vec!["metric_a".to_string()],
            rows: vec![test_record(
                2_000,
                HashMap::from([("metric_a".to_string(), 2.0)]),
            )],
            pending_row_count: 1,
            rewrite_existing: false,
        };

        write_metric_rows_csv(&csv_path, &first).unwrap();
        write_metric_rows_csv(&csv_path, &second).unwrap();

        let contents = fs::read_to_string(&csv_path).unwrap();
        assert_eq!(
            contents,
            "timestamp,source_kind,metric_a\n1000,agent_websocket,1\n2000,agent_websocket,2\n"
        );
        let _ = fs::remove_file(csv_path);
    }

    #[test]
    fn metric_csv_rewrites_header_once_when_schema_expands() {
        let csv_path = temp_metric_csv_path("schema_expand");
        let first = MetricFlushSnapshot {
            metric_names: vec!["metric_a".to_string()],
            previous_metric_names: Vec::new(),
            rows: vec![test_record(
                1_000,
                HashMap::from([("metric_a".to_string(), 1.0)]),
            )],
            pending_row_count: 1,
            rewrite_existing: false,
        };
        let expanded = MetricFlushSnapshot {
            metric_names: vec!["metric_a".to_string(), "metric_b".to_string()],
            previous_metric_names: vec!["metric_a".to_string()],
            rows: vec![test_record(
                2_000,
                HashMap::from([
                    ("metric_a".to_string(), 2.0),
                    ("metric_b".to_string(), 20.0),
                ]),
            )],
            pending_row_count: 1,
            rewrite_existing: true,
        };

        write_metric_rows_csv(&csv_path, &first).unwrap();
        write_metric_rows_csv(&csv_path, &expanded).unwrap();

        let contents = fs::read_to_string(&csv_path).unwrap();
        assert_eq!(
            contents,
            "timestamp,source_kind,metric_a,metric_b\n1000,agent_websocket,1,NaN\n2000,agent_websocket,2,20\n"
        );
        let _ = fs::remove_file(csv_path);
    }
}

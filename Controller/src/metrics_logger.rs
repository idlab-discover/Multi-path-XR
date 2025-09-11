use std::{collections::HashMap, fs, io::Write, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use polars::prelude::*;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info};
use dashmap::DashMap;

const PROMETHEUS_URL: &str = "http://0.0.0.0:9090";
// scrape every second
const PERIOD: Duration = Duration::from_secs(1);
 // keep the last minute at 1Hz
const IN_MEMORY_WINDOW: usize = 60;
// shave up to 75% of a period to catch up (bounded correction)
const CATCHUP_FRACTION: f64 = 0.75;
// only skip if we're essentially a whole second late
const SKIP_THRESHOLD: Duration = Duration::from_millis(950);

fn sanitize_label_value(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum MetricsLoggerError {
    Reqwest(reqwest::Error),
    Io(std::io::Error),
    Polars(polars::error::PolarsError),
    Serde(serde_json::Error),
    MissingData,
    AlreadyRunning,
    NotRunning,
    LoggerNotInitialized
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

impl From<polars::error::PolarsError> for MetricsLoggerError {
    fn from(err: polars::error::PolarsError) -> Self {
        MetricsLoggerError::Polars(err)
    }
}

impl From<serde_json::Error> for MetricsLoggerError {
    fn from(err: serde_json::Error) -> Self {
        MetricsLoggerError::Serde(err)
    }
}

#[derive(Clone)]
pub struct MetricsLogger {
    folder_path: PathBuf,
    client: Client,
    // Sharded map + per-instance RwLock to reduce contention
    dataframes: Arc<DashMap<String, Arc<RwLock<DataFrame>>>>,
    all_metrics: Vec<String>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    task_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl MetricsLogger {
    pub async fn new(experiment_name: &str) -> Result<Self, MetricsLoggerError> {
        // Create copy of the experiment file
        let path = PathBuf::from(format!("./dist/experiments/{experiment_name}"));
        if !path.exists() {
            fs::create_dir_all(&path)?;
        }
        let contents = std::fs::read_to_string(&path)?;

        let start_time = Utc::now().timestamp_millis();
        let sanitized_name = experiment_name.replace('.', "_");
        let folder_path: PathBuf = ["metrics", "measurements", &sanitized_name, &start_time.to_string()].iter().collect();
        fs::create_dir_all(&folder_path)?;

        // Write a copy of the experiment file
        let experiment_file_path = folder_path.join(format!("experiment_{start_time}.yaml"));
        let mut file = fs::File::create(&experiment_file_path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;

        let client = Client::new();
        let metrics = Self::fetch_all_metrics(&client).await?;

        info!("[metrics_logger] Saving CSV files to {:?}", folder_path);
        info!("[metrics_logger] Found {} metrics.", metrics.len());

        Ok(Self {
            folder_path,
            client,
            dataframes: Arc::new(DashMap::new()),
            all_metrics: metrics,
            shutdown_tx: Arc::new(Mutex::new(None)),
            task_handle: Arc::new(Mutex::new(None)),
        })
    }

    async fn fetch_all_metrics(client: &Client) -> Result<Vec<String>, MetricsLoggerError> {
        let url = format!("{PROMETHEUS_URL}/api/v1/label/__name__/values");
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
            if e.is_connect() {
                // Connection refused or network error: return empty metrics list
                return Ok(Vec::new());
            } else {
                return Err(MetricsLoggerError::Reqwest(e));
            }
            }
        };
        let json: Value = resp.json().await?;
        let mut metrics: Vec<String> = json["data"].as_array()
            .ok_or(MetricsLoggerError::MissingData)?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        metrics.sort();
        Ok(metrics)
    }

    async fn refresh_metrics(&mut self) -> Result<(), MetricsLoggerError> {
        let metrics = Self::fetch_all_metrics(&self.client).await?;
        // Instead of overwriting, append missing metrics to the existing vec.
        // This vec must not contain duplicates.
        let new_metrics: Vec<String> = metrics.into_iter().filter(|m| !self.all_metrics.contains(m)).collect();
        self.all_metrics.extend(new_metrics);
        Ok(())
    }

    async fn query_metric(&self, metric_name: &str) -> Result<Vec<Value>, MetricsLoggerError> {
        let url: String = format!("{PROMETHEUS_URL}/api/v1/query");
        let resp = self.client.get(url).query(&[("query", metric_name)]).send().await?;
        let json: Value = resp.json().await?;
        Ok(json["data"]["result"].as_array().cloned().unwrap_or_default())
    }

    // Batch query: fetch multiple metrics at once via regex on __name__
    async fn query_metrics_batch(&self, metric_names: &[String]) -> Result<Vec<Value>, MetricsLoggerError> {
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
        let mut shutdown_guard = self.shutdown_tx.lock().await;
        if shutdown_guard.is_some() {
            return Err(MetricsLoggerError::AlreadyRunning);
        }
        let (tx, rx) = watch::channel(false);
        *shutdown_guard = Some(tx);

        let self_clone = self.clone();
        let mut self_mutable = self_clone.clone();
        let handle = tokio::spawn(async move {
            // Anchor the schedule to now and keep a tick index
            let start = tokio::time::Instant::now();
            let mut tick_idx: u64 = 1;
            loop {
                if *rx.borrow() { break; }

                if tick_idx % 5 == 0 {
                    if let Err(e) = self_mutable.refresh_metrics().await {
                        error!("[metrics_logger] Error refreshing metrics: {:?}", e);
                    }
                }

                if let Err(e) = self_clone.collect_and_write().await {
                    error!("[metrics_logger] Error: {:?}", e);
                }

                // ---- Drift-resistant timing with bounded catch-up ----
                let now = tokio::time::Instant::now();
                // Target time for *this* tick (still the one we just executed)
                let target = start + PERIOD * (tick_idx as u32);

                if now < target {
                    // Early: sleep exactly until the grid time
                    tokio::time::sleep_until(target).await;
                    tick_idx = tick_idx.saturating_add(1);
                    continue;
                }

                // Late: how late are we relative to the grid?
                let lateness = now.saturating_duration_since(target);

                if lateness < SKIP_THRESHOLD {
                    // Prefer not to skip: shave some time off the *next* sleep (bounded)
                    let catchup_cap = PERIOD.mul_f64(CATCHUP_FRACTION);
                    let shave = if lateness > catchup_cap { catchup_cap } else { lateness };
                    let sleep_dur = PERIOD.saturating_sub(shave);
                    if !sleep_dur.is_zero() {
                        sleep(sleep_dur).await;
                    }
                    tick_idx = tick_idx.saturating_add(1);
                    continue;
                }

                // Very late (≈ full second or more): snap to current grid slot (single skip)
                let elapsed = now - start;
                let full_ticks = (elapsed.as_nanos() / PERIOD.as_nanos()) as u64;
                tick_idx = full_ticks + 1;
                // no sleep; immediately loop and measure again
            }
        });

        let mut task_guard = self.task_handle.lock().await;
        *task_guard = Some(handle);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), MetricsLoggerError> {
        let mut shutdown_guard = self.shutdown_tx.lock().await;
        let mut task_guard = self.task_handle.lock().await;

        if let Some(tx) = shutdown_guard.take() {
            let _ = tx.send(true);
        } else {
            return Err(MetricsLoggerError::NotRunning);
        }

        if let Some(handle) = task_guard.take() {
            let _ = handle.await;
        }

        Ok(())
    }

    async fn collect_and_write(&self) -> Result<(), MetricsLoggerError> {
        let timestamp: DateTime<Utc> = Utc::now();
        let mut step_data: HashMap<String, HashMap<String, f64>> = HashMap::new();

        // Batch query all metrics (instant vector). Use __name__ from each series.
        let results = self.query_metrics_batch(&self.all_metrics).await?;
        for res in results {
            let metric_obj = &res["metric"];
            // instant vector: "value" = [timestamp, "val"]
            let value_arr = &res["value"];
            let instance = metric_obj.get("instance").and_then(Value::as_str).unwrap_or("unknown");
            let mode = metric_obj.get("mode").and_then(Value::as_str).unwrap_or("unknown");
            let metric_name = metric_obj.get("__name__").and_then(Value::as_str).unwrap_or("unknown");
            // If present, fold stream_id into the instance key to keep per-stream series separate
            let stream_id = metric_obj.get("stream_id").and_then(Value::as_str);
            let instance_name = if let Some(sid) = stream_id {
                format!("{instance}_{mode}_sid_{}", sanitize_label_value(sid))
            } else {
                format!("{instance}_{mode}")
            };
            let value = value_arr.get(1).and_then(Value::as_str).unwrap_or("0.0")
                .parse::<f64>().unwrap_or(f64::NAN);
            step_data.entry(instance_name).or_default().insert(metric_name.to_string(), value);
        }

        for (instance, metrics) in step_data {
            let csv_path = self.folder_path.join(format!("metrics_{}.csv", instance.replace(':', "_")));

            debug!("[metrics_logger] Writing metrics for instance: {}", instance);

            // ---- Build row outside any locks ----
            let mut cols: Vec<Column> = Vec::with_capacity(self.all_metrics.len() + 1);
            cols.push(Column::new("timestamp".into(), &[timestamp.timestamp_millis()]));
            for m in &self.all_metrics {
                let val = *metrics.get(m).unwrap_or(&f64::NAN);
                cols.push(Column::new(m.into(), &[val]));
            }
            let new_df = DataFrame::new(cols)?;

            // Pre-encode CSV bytes outside locks
            let mut bytes = Vec::new();
            let include_header = !csv_path.exists();
            {
                let mut tmp = new_df.clone();
                CsvWriter::new(&mut bytes).include_header(include_header).finish(&mut tmp)?;
            }

            // ---- Per-instance lock only for quick mutations ----
            // Get or create the per-instance DataFrame lock (do not hold DashMap ref across await)
            let df_lock = if let Some(entry) = self.dataframes.get(&instance) {
                entry.value().clone()
            } else {
                // Create new entry
                let lock = Arc::new(RwLock::new(DataFrame::default()));
                self.dataframes.insert(instance.clone(), lock.clone());
                lock
            };

            {
                let mut df = df_lock.write().await;
                // Ensure schema (timestamp + all current metrics). Keep this short.
                // Add missing timestamp column if needed
                if !df.get_column_names().iter().any(|&c| c == "timestamp") {
                    let ts_col = Series::new("timestamp".into(), vec![0_i64; df.height()]);
                    df.with_column(ts_col)?;
                }
                // Add missing metrics as NaN columns
                for m in &self.all_metrics {
                    if !df.get_column_names().iter().any(|&c| c == m) {
                        let series = Series::new(m.into(), vec![f64::NAN; df.height()]);
                        df.with_column(series)?;
                    }
                }
                df.vstack_mut(&new_df)?;
                if df.height() > IN_MEMORY_WINDOW {
                    *df = df.tail(Some(IN_MEMORY_WINDOW));
                }
            } // df_lock released here

            // ---- Append CSV after releasing locks ----
            let mut f = fs::OpenOptions::new().create(true).append(true).open(csv_path)?;
            f.write_all(&bytes)?;
        }
        Ok(())
    }

    // Get the last N time-series pairs (timestamp_ms, value) for a given instance+metric
    pub async fn get_last_n(
        &self,
        instance: &str,
        metric: &str,
        n: usize,
    ) -> Result<Vec<(i64, f64)>, MetricsLoggerError> {
        let df_lock = self
            .dataframes
            .get(instance)
            .ok_or(MetricsLoggerError::MissingData)?
            .value()
            .clone();
        // Only lock the instance you need
        let df = df_lock.read().await;

        // Ensure columns exist
        let ts_col = df.column("timestamp")?;
        let val_col = df.column(metric)?;

        // Cast/borrow as the right types
        let ts = ts_col.i64().map_err(|_| MetricsLoggerError::MissingData)?;
        let vals = val_col.f64().map_err(|_| MetricsLoggerError::MissingData)?;

        // Compute window
        let len = df.height();
        let start = len.saturating_sub(n);
        let mut out = Vec::with_capacity(n.min(len));

        // Walk in lock-step
        for i in start..len {
            // Safe due to shape equality enforced by DataFrame
            let t = ts.get(i).unwrap_or(0);
            let v = vals.get(i).unwrap_or(f64::NAN);
            out.push((t, v));
        }
        Ok(out)
    }
}

use std::{collections::HashMap, fs, io::Write, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use polars::prelude::*;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info};

const PROMETHEUS_URL: &str = "http://0.0.0.0:9090";

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
    dataframes: Arc<Mutex<HashMap<String, DataFrame>>>,
    all_metrics: Vec<String>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    task_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl MetricsLogger {
    pub async fn new(experiment_name: &str) -> Result<Self, MetricsLoggerError> {
        // Create copy of the experiment file
        let path = format!("./dist/experiments/{}", experiment_name);
        let path = PathBuf::from(path);
        if !path.exists() {
            fs::create_dir_all(&path)?;
        }
        let contents = std::fs::read_to_string(&path)?;

        let start_time = Utc::now().timestamp_millis();
        let sanitized_name = experiment_name.replace('.', "_");
        let folder_path: PathBuf = ["metrics", "measurements", &sanitized_name, &start_time.to_string()].iter().collect();
        fs::create_dir_all(&folder_path)?;

        // Write a copy of the experiment file
        let experiment_file_path = folder_path.join(format!("experiment_{}.yaml", start_time));
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
            dataframes: Arc::new(Mutex::new(HashMap::new())),
            all_metrics: metrics,
            shutdown_tx: Arc::new(Mutex::new(None)),
            task_handle: Arc::new(Mutex::new(None)),
        })
    }

    async fn fetch_all_metrics(client: &Client) -> Result<Vec<String>, MetricsLoggerError> {
        let url = format!("{}/api/v1/label/__name__/values", PROMETHEUS_URL);
        let resp = client.get(url).send().await?;
        let json: Value = resp.json().await?;
        Ok(json["data"].as_array()
            .ok_or(MetricsLoggerError::MissingData)?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    async fn refresh_metrics(&mut self) -> Result<(), MetricsLoggerError> {
        let metrics = Self::fetch_all_metrics(&self.client).await?;
        self.all_metrics = metrics;
        Ok(())
    }

    async fn query_metric(&self, metric_name: &str) -> Result<Vec<Value>, MetricsLoggerError> {
        let url = format!("{}/api/v1/query", PROMETHEUS_URL);
        let resp = self.client.get(url).query(&[("query", metric_name)]).send().await?;
        let json: Value = resp.json().await?;
        Ok(json["data"]["result"].as_array().cloned().unwrap_or_default())
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
            let mut i = 0;
            loop {
                if *rx.borrow() { break; }

                if i % 5 == 0 {
                    if let Err(e) = self_mutable.refresh_metrics().await {
                        error!("[metrics_logger] Error refreshing metrics: {:?}", e);
                    }
                }

                if let Err(e) = self_clone.collect_and_write().await {
                    error!("[metrics_logger] Error: {:?}", e);
                }
                sleep(Duration::from_secs(1)).await;

                i += 1;
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

        for metric in &self.all_metrics {
            let results = self.query_metric(metric).await?;
            for res in results {
                let metric_obj = &res["metric"];
                let value_arr = &res["value"];
                let instance = metric_obj.get("instance").and_then(Value::as_str).unwrap_or("unknown");
                let mode = metric_obj.get("mode").and_then(Value::as_str).unwrap_or("unknown");
                let instance_name = format!("{}_{}", instance, mode);
                let value = value_arr[1].as_str().unwrap_or("0.0").parse::<f64>().unwrap_or(f64::NAN);
                step_data.entry(instance_name).or_default().insert(metric.clone(), value);
            }
        }

        let mut dfs = self.dataframes.lock().await;

        for (instance, metrics) in step_data {
            let csv_path = self.folder_path.join(format!("metrics_{}.csv", instance.replace(':', "_")));

            debug!("[metrics_logger] Writing metrics for instance: {}", instance);

            // create DataFrame if it doesn't exist
            let df = dfs.entry(instance.clone()).or_insert_with(DataFrame::default);
            // add missing timestamp column first
            if !df.get_column_names().iter().any(|&c| c == "timestamp") {
                let ts_col = Series::new("timestamp".into(), vec![0_i64; df.height()]);
                df.with_column(ts_col)?;
            }
            // add missing metrics columns
            for m in &self.all_metrics {
                if !df.get_column_names().iter().any(|&c| c == m) {
                    let series = Series::new(m.into(), vec![f64::NAN; df.height()]);
                    df.with_column(series)?;
                }
            }

            // build new row with all metrics in consistent order
            let mut cols: Vec<Column> = Vec::with_capacity(self.all_metrics.len() + 1);
            cols.push(Column::new("timestamp".into(), &[timestamp.timestamp_millis()]));
            for m in &self.all_metrics {
                let val = *metrics.get(m).unwrap_or(&f64::NAN);
                cols.push(Column::new(m.into(), &[val]));
            }
            
            let mut new_df = DataFrame::new(cols)?;
            df.vstack_mut(&new_df)?;
            if df.height() > 5 { *df = df.tail(Some(5)); }

            // append CSV
            let mut bytes = Vec::new();
            let include_header = !csv_path.exists();
            CsvWriter::new(&mut bytes).include_header(include_header).finish(&mut new_df)?;
            let mut f = fs::OpenOptions::new().create(true).append(true).open(csv_path)?;
            f.write_all(&bytes)?;
        }
        Ok(())
    }
}

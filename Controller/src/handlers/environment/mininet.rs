use super::EnvironmentHandler;
use async_trait::async_trait;
use tracing::{info, error};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::process::Stdio;
use reqwest::Client;
use tokio::task;
use tokio::process::{Command, Child};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone)]
pub struct MininetHandler {
    process: Arc<Mutex<Option<Child>>>,
    client: Client,
    base_url: String,
}

impl MininetHandler {
    pub fn new() -> Self {
        MininetHandler {
            process: Arc::new(Mutex::new(None)),
            client: Client::new(),
            base_url: "http://127.0.0.1:5000".to_string(), // Adjust if your Mininet server runs on a different address
        }
    }

    async fn ensure_server_running(&self) -> Result<(), String> {
        let mut process_guard = self.process.lock().await;
        if process_guard.is_some() {
            return Ok(()); // Server is already running
        }

        info!("Starting Mininet server process");
        // Start the Mininet server process
        let mut command = Command::new("../run.sh");
        command
            .arg("--mininet") // Path to your Mininet server script
            .arg("--no-clear")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        match command.spawn() {
            Ok(mut child) => { // Set up logging for stdout
                if let Some(stdout) = child.stdout.take() {
                    let mut reader = BufReader::new(stdout).lines();
                    let log_prefix = format!("\x1b[38;5;36m[{}]\x1b[0m", "Mininet").to_string();
                    task::spawn(async move {
                        while let Ok(Some(line)) = reader.next_line().await {
                            info!("{} {}", log_prefix, line);
                        }
                    });
                }

                // Set up logging for stderr
                if let Some(stderr) = child.stderr.take() {
                    let mut reader = BufReader::new(stderr).lines();
                    let log_prefix = format!("\x1b[38;5;36m[{}]\x1b[0m", "Mininet").to_string();
                    task::spawn(async move {
                        while let Ok(Some(line)) = reader.next_line().await {
                            error!("{} {}", log_prefix, line);
                        }
                    });
                }

                *process_guard = Some(child);
                // Wait a bit for the server to start
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                Ok(())
            }
            Err(e) => {
                error!("Failed to start Mininet server: {:?}", e);
                Err(format!("Failed to start Mininet server: {:?}", e))
            }
        }
    }

    async fn stop_server(&self) -> Result<(), String> {
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            info!("Stopping Mininet server process");
            match child.kill().await {
                Ok(_) => {
                    let _ = child.wait().await;
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to stop Mininet server: {:?}", e);
                    Err(format!("Failed to stop Mininet server: {:?}", e))
                }
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for MininetHandler {
    fn drop(&mut self) {
        let process_clone = self.process.clone();
        tokio::spawn(async move {
            let mut process_guard = process_clone.lock().await;
            if let Some(mut child) = process_guard.take() {
                info!("Dropping MininetHandler and stopping server");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        });
    }
}

#[async_trait]
impl EnvironmentHandler for MininetHandler {
    async fn start(&self, options: &str) -> Result<String, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/start", self.base_url);
        // Parse the options string into query parameters
        let options_params: Vec<(&str, &str)> = options
            .split('&')
            .filter_map(|s| {
                let mut iter = s.splitn(2, '=');
                if let (Some(k), Some(v)) = (iter.next(), iter.next()) {
                    Some((k, v))
                } else {
                    None
                }
            })
            .collect();
        let response = self.client.get(&url).query(&options_params).send().await;
        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let json: Value = resp.json().await.unwrap_or_default();
                    Ok(json["message"].as_str().unwrap_or("Mininet started").to_string())
                } else {
                    let err = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to start Mininet: {}", e)),
        }
    }

    async fn stop(&self) -> Result<String, String> {
        let url = format!("{}/stop", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    self.stop_server().await?;
                    let json: Value = resp.json().await.unwrap_or_default();
                    Ok(json["message"].as_str().unwrap_or("Mininet stopped").to_string())
                } else {
                    let err = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to stop Mininet: {}", e)),
        }
    }

    async fn exec(&self, params: HashMap<String, String>) -> Result<String, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/exec", self.base_url);
        let response = self.client.get(&url).query(&params).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    Ok(text)
                } else {
                    let err = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to execute command: {}", e)),
        }
    }

    async fn nodes(&self) -> Result<Value, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/nodes", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get nodes: {}", e)),
        }
    }

    async fn links(&self) -> Result<Value, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/links", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get links: {}", e)),
        }
    }

    async fn status(&self) -> Result<Value, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/status", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get status: {}", e)),
        }
    }

    async fn visualize(&self) -> Result<Vec<u8>, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/visualize", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
                    Ok(bytes.to_vec())
                } else {
                    let err = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to get visualization: {}", e)),
        }
    }

    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/start_xterm", self.base_url);
        let response = self.client.get(&url).query(&params).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let json: Value = resp.json().await.unwrap_or_default();
                    Ok(json["message"].as_str().unwrap_or("Xterm started").to_string())
                } else {
                    let json: Value = resp.json().await.unwrap_or_default();
                    Err(json["error"].as_str().unwrap_or("Unknown error").to_string())
                }
            }
            Err(e) => Err(format!("Failed to start xterm: {}", e)),
        }
    }

    async fn ping_all(&self) -> Result<Value, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/ping_all", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to ping all: {}", e)),
        }
    }
}

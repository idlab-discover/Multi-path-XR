use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::error::{Result, VirtualWallError};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExperimentSummary {
    pub id: String,
    pub friendly_name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceSummary {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub friendly_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub site_id: Option<String>,
    #[serde(default)]
    pub ipv4: Option<String>,
    #[serde(default)]
    pub ipv6: Option<String>,
    #[serde(default)]
    pub private_ipv4: Option<String>,
    #[serde(default)]
    pub private_ipv6: Option<String>,
    #[serde(default)]
    pub public_ipv4: Option<String>,
    #[serde(default)]
    pub public_ipv6: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BiNetworkInterface {
    #[serde(default)]
    pub port_id: Option<String>,
    #[serde(default)]
    pub network_id: Option<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceDetail {
    pub id: Option<String>,
    pub friendly_name: Option<String>,
    pub status: Option<String>,
    pub site_id: Option<String>,
    pub private_ipv4: Option<String>,
    pub private_ipv6: Option<String>,
    pub public_ipv4: Option<String>,
    pub public_ipv6: Option<String>,
    #[serde(default)]
    pub network_interfaces: Vec<BiNetworkInterface>,
    #[serde(default)]
    pub ssh_logins: Vec<SshLogin>,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SshLogin {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub jump_proxy: Option<SshProxy>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SshProxy {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
}

fn default_ssh_port() -> u16 {
    22
}

fn format_command(args: &[String]) -> String {
    args.join(" ")
}

pub struct SlicesClient {
    binary: PathBuf,
    env: HashMap<String, String>,
    /// If set, every `slices bi ...` call is executed as `slices bi --infra <id> ...`.
    bi_infra_id: Option<String>,
}

impl SlicesClient {
    pub fn new(binary: PathBuf) -> Self {
        Self {
            binary,
            env: HashMap::new(),
            bi_infra_id: None,
        }
    }

    /// Set the BI infra id used for all `slices bi` commands.
    ///
    /// This is required for some SLICES CLI versions (notably when using custom BI configs)
    /// where `create-from-file` otherwise routes via an orchestrator that doesn't know your infra.
    pub fn set_bi_infra_id(&mut self, infra_id: Option<String>) {
        self.bi_infra_id = infra_id;
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.env.insert(key.into(), value.into());
    }

    fn bi_prefix_args(&self) -> Vec<String> {
        let mut args = vec!["bi".to_string()];
        if let Some(infra) = &self.bi_infra_id {
            // Must appear BEFORE the `bi` subcommand (e.g. create-from-file).
            args.push("--infra".to_string());
            args.push(infra.clone());
        }
        args
    }

    fn bi_args_from(
        mut prefix: Vec<String>,
        tail: impl IntoIterator<Item = String>,
    ) -> Vec<String> {
        prefix.extend(tail);
        prefix
    }

    pub async fn ensure_project(&self, project: &str) -> Result<()> {
        self.run_raw(["project", "use", project]).await?;
        Ok(())
    }

    pub async fn ensure_experiment(
        &self,
        project: Option<&str>,
        experiment: &str,
        duration: Option<&str>,
    ) -> Result<ExperimentSummary> {
        if let Some(project) = project {
            self.ensure_project(project).await?;
        }

        if let Some(existing) = self.try_experiment_show(experiment).await? {
            return Ok(existing);
        }

        let mut args = vec![
            "experiment".to_string(),
            "create".to_string(),
            experiment.to_string(),
        ];
        if let Some(duration) = duration {
            args.push("--duration".to_string());
            args.push(duration.to_string());
        }
        let output = self.run_raw(args).await?;
        info!("Created SLICES experiment `{experiment}`");
        match serde_json::from_str::<ExperimentSummary>(&output.stdout) {
            Ok(summary) => Ok(summary),
            Err(_) => self.try_experiment_show(experiment).await?.ok_or_else(|| {
                VirtualWallError::CliOutput {
                    command: "experiment create".to_string(),
                    message: "unable to parse creation output".to_string(),
                }
            }),
        }
    }

    pub async fn try_experiment_show(&self, experiment: &str) -> Result<Option<ExperimentSummary>> {
        match self
            .run_json(["experiment", "show", experiment, "--format", "json"])
            .await
        {
            Ok(value) => {
                let summary: ExperimentSummary = serde_json::from_value(value)?;
                Ok(Some(summary))
            }
            Err(VirtualWallError::CliFailure { status: 1, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn experiment_extend(
        &self,
        experiment: &str,
        duration: &str,
    ) -> Result<ExperimentSummary> {
        let value = self
            .run_json([
                "experiment",
                "extend",
                experiment,
                "--duration",
                duration,
                "--format",
                "json",
            ])
            .await?;
        let summary: ExperimentSummary = serde_json::from_value(value)?;
        Ok(summary)
    }

    pub async fn bi_create_from_file(
        &self,
        spec_path: &Path,
        wait: bool,
        experiment: &str,
    ) -> Result<CommandOutput> {
        let mut tail = vec![
            "create-from-file".to_string(),
            spec_path.as_os_str().to_string_lossy().to_string(),
            "--experiment".to_string(),
            experiment.to_string(),
        ];
        if wait {
            tail.push("--wait".to_string());
        }
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let out = self.run_raw(args).await?;
        Ok(out)
    }

    pub async fn bi_list(&self, experiment: &str) -> Result<String> {
        let mut tail = vec![
            "list".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ];

        if !experiment.trim().is_empty() {
            tail.append(&mut vec![
                "--experiment".to_string(),
                experiment.to_string(),
            ]);
        }

        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let out = self.run_raw(args).await?;
        Ok(out.stdout)
    }

    pub async fn bi_show(&self, resource_id: &str) -> Result<ResourceSummary> {
        let tail = vec![
            "show".to_string(),
            resource_id.to_string(),
            "--format".to_string(),
            "json".to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let value = self.run_json(args).await?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub async fn bi_show_with_experiment(
        &self,
        resource_id: &str,
        experiment: &str,
    ) -> Result<ResourceDetail> {
        let tail = vec![
            "show".to_string(),
            resource_id.to_string(),
            "--experiment".to_string(),
            experiment.to_string(),
            "--format".to_string(),
            "json".to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let value = self.run_json(args).await?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub async fn bi_delete(&self, resource_id: &str) -> Result<()> {
        let tail = vec!["delete".to_string(), resource_id.to_string()];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        match self.run_raw(args).await {
            Ok(output) => {
                debug!("Deleted resource {resource_id}: {}", output.stdout.trim());
                Ok(())
            }
            Err(err @ VirtualWallError::CliFailure { .. }) => {
                warn!("Failed to delete resource {resource_id}: {err:?}");
                Err(err)
            }
            Err(err) => Err(err),
        }
    }

    pub async fn experiment_list_resources(
        &self,
        experiment: &str,
    ) -> Result<Vec<ResourceSummary>> {
        let tail = vec![
            "list-resources".to_string(),
            "--experiment".to_string(),
            experiment.to_string(),
            "--format".to_string(),
            "json".to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);

        let value = self.run_json(args).await?;
        if value.is_array() {
            Ok(serde_json::from_value(value)?)
        } else {
            Err(VirtualWallError::CliOutput {
                command: "experiment list-resources".to_string(),
                message: "expected array output".to_string(),
            })
        }
    }

    pub async fn bi_ssh_show_json(&self, resource: &str, experiment: &str) -> Result<SshLogin> {
        let tail = vec![
            "ssh".to_string(),
            resource.to_string(),
            "--experiment".to_string(),
            experiment.to_string(),
            "--show".to_string(),
            "json".to_string(),
            "--no-exec".to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let out = self.run_raw(args).await?;
        serde_json::from_str::<SshLogin>(&out.stdout).map_err(Into::into)
    }

    pub async fn run_json<I, S>(&self, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let result = self.run_raw(args).await?;
        let value: Value = serde_json::from_str(&result.stdout)?;
        Ok(value)
    }

    pub async fn run_raw<I, S>(&self, args: I) -> Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args_vec: Vec<String> = args
            .into_iter()
            .map(|s| s.as_ref().to_string_lossy().to_string())
            .collect();
        info!("slices cmd: {args_vec:?}");

        let output = self
            .prepare_command(&args_vec)
            .output()
            .await
            .map_err(VirtualWallError::from)?;
        let status = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !stdout.trim().is_empty() {
            info!("slices cmd stdout: {}", stdout.trim());
        }
        if !stderr.trim().is_empty() {
            info!("slices cmd stderr: {}", stderr.trim());
        }

        if !output.status.success() {
            return Err(VirtualWallError::CliFailure {
                command: format_command(&args_vec),
                status,
                stderr,
            });
        }

        Ok(CommandOutput { stdout, stderr })
    }

    pub(crate) fn prepare_command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.binary);
        command.arg("--no-upgrade-check").args(args);
        // Ensure PATH contains the slices binary directory so slices-core is discoverable.
        if let Some(dir) = self.binary.parent() {
            let mut path_val = std::env::var("PATH").unwrap_or_default();

            let dir_str = dir.to_string_lossy().to_string();
            if !path_val.split(':').any(|p| p == dir_str) {
                path_val = format!("{dir_str}:{path_val}");
            }
            command.env("PATH", path_val);
        }
        if !self.env.is_empty() {
            command.envs(&self.env);
        }

        //debug!("Prepared SLICES command: {:?}", command);

        command
    }

    /// Extend a BI resource lifetime by duration (e.g. `"2d"`, `"6h"`).
    pub async fn bi_extend(&self, name_or_id: &str, duration: &str) -> Result<()> {
        if name_or_id.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "name_or_id must be non-empty".into(),
            ));
        }
        if duration.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "duration must be non-empty".into(),
            ));
        }
        let tail = vec![
            "extend".to_string(),
            name_or_id.to_string(),
            "--duration".to_string(),
            duration.to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let _ = self.run_raw(args).await?;
        Ok(())
    }

    /// Reset a BI resource.
    pub async fn bi_reset(&self, name_or_id: &str) -> Result<()> {
        if name_or_id.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "name_or_id must be non-empty".into(),
            ));
        }
        let tail = vec!["reset".to_string(), name_or_id.to_string()];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let _ = self.run_raw(args).await?;
        Ok(())
    }

    /// Destroy multiple BI resources in an experiment.
    pub async fn bi_destroy(&self, experiment: &str, names_or_ids: &[String]) -> Result<()> {
        if experiment.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "experiment must be non-empty".into(),
            ));
        }
        if names_or_ids.is_empty() {
            return Ok(());
        }

        let mut tail = vec![
            "destroy".to_string(),
            "--experiment".to_string(),
            experiment.to_string(),
        ];
        tail.extend(names_or_ids.iter().cloned());
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let _ = self.run_raw(args).await?;
        Ok(())
    }

    /// Run a command via `slices bi ssh <node> -- <cmd>`.
    pub async fn bi_ssh(&self, name_or_id: &str, cmd: &str) -> Result<CommandOutput> {
        if name_or_id.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "name_or_id must be non-empty".into(),
            ));
        }
        if cmd.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "cmd must be non-empty".into(),
            ));
        }
        let tail = vec![
            "ssh".to_string(),
            name_or_id.to_string(),
            "--".to_string(),
            cmd.to_string(),
        ];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let out = self.run_raw(args).await?;
        Ok(out)
    }

    /// Copy files via `slices bi scp <src> <dst>`.
    pub async fn bi_scp(&self, src: &str, dst: &str) -> Result<()> {
        if src.trim().is_empty() || dst.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "src/dst must be non-empty".into(),
            ));
        }
        let tail = vec!["scp".to_string(), src.to_string(), dst.to_string()];
        let args = Self::bi_args_from(self.bi_prefix_args(), tail);
        let _ = self.run_raw(args).await?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
}

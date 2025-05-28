pub mod docker;
pub mod mininet;
pub mod virtual_wall;

pub use docker::DockerHandler;
pub use mininet::MininetHandler;
pub use virtual_wall::VirtualWallHandler;

use async_trait::async_trait;
use dyn_clone::DynClone;
use serde_json::Value;
use std::collections::HashMap;

#[async_trait]
pub trait EnvironmentHandler: DynClone + Send + Sync {
    async fn start(&self, options: &str) -> Result<String, String>;
    async fn stop(&self) -> Result<String, String>;
    async fn exec(&self, params: HashMap<String, String>) -> Result<String, String>;
    async fn nodes(&self) -> Result<Value, String>;
    async fn links(&self) -> Result<Value, String>;
    async fn status(&self) -> Result<Value, String>;
    async fn visualize(&self) -> Result<Vec<u8>, String>;
    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String>;
    async fn ping_all(&self) -> Result<Value, String>;
}

// Enable cloning of trait objects
// dyn_clone::clone_trait_object!(EnvironmentHandler);
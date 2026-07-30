pub mod big_virtual_wall;
pub mod docker;
pub mod mininet;
pub mod virtual_wall;
pub mod virtual_wall_lite;

pub use big_virtual_wall::BigVirtualWallHandler;
pub use docker::DockerHandler;
pub use mininet::MininetHandler;
pub use virtual_wall::VirtualWallHandler;
pub use virtual_wall_lite::VirtualWallLiteHandler;

use async_trait::async_trait;
use dyn_clone::DynClone;
use serde_json::Value;
use std::collections::HashMap;

#[async_trait]
pub trait EnvironmentHandler: DynClone + Send + Sync {
    async fn start(&self, options: &str) -> Result<String, String>;
    async fn stop(&self) -> Result<String, String>;
    async fn cleanup_processes(&self) -> Result<String, String> {
        Ok("Environment-specific process cleanup not needed".to_string())
    }
    async fn exec(&self, params: HashMap<String, String>) -> Result<String, String>;
    async fn nodes(&self) -> Result<Value, String>;
    async fn links(&self) -> Result<Value, String>;
    async fn status(&self) -> Result<Value, String>;
    async fn visualize(&self) -> Result<Vec<u8>, String>;
    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String>;
    async fn ping_all(&self) -> Result<Value, String>;
    async fn open_tunnel(&self, _params: HashMap<String, String>) -> Result<Value, String> {
        Err("Tunneling not supported for this environment".to_string())
    }
    async fn close_tunnel(&self, _id: &str) -> Result<String, String> {
        Err("Tunneling not supported for this environment".to_string())
    }
    async fn list_tunnels(&self) -> Result<Value, String> {
        Err("Tunneling not supported for this environment".to_string())
    }
}

// Enable cloning of trait objects
// dyn_clone::clone_trait_object!(EnvironmentHandler);

use plotters::style::{register_font, FontStyle};
use std::sync::OnceLock;

#[derive(Debug)]
pub enum FontInitError {
    #[allow(dead_code)]
    InvalidEmbeddedFont {
        name: &'static str,
        style: String,
        details: String,
    },
}

static FONT_INIT: OnceLock<Result<(), FontInitError>> = OnceLock::new();

/// Initializes Plotters fonts for headless environments (safe to call multiple times).
pub fn ensure_plotters_fonts() -> Result<(), FontInitError> {
    match FONT_INIT.get_or_init(|| {
        // Adjust paths to match your crate layout.
        const REGULAR: &[u8] = include_bytes!("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
        const BOLD: &[u8] = include_bytes!("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf");

        register_font("sans-serif", FontStyle::Normal, REGULAR).map_err(|_e| {
            FontInitError::InvalidEmbeddedFont {
                name: "sans-serif",
                style: "normal".into(),
                details: "error registering regular font".into(),
            }
        })?;

        register_font("sans-serif", FontStyle::Bold, BOLD).map_err(|_e| {
            FontInitError::InvalidEmbeddedFont {
                name: "sans-serif",
                style: "bold".into(),
                details: "error registering bold font".into(),
            }
        })?;

        Ok(())
    }) {
        Ok(()) => Ok(()),
        Err(e) => Err(FontInitError::InvalidEmbeddedFont {
            name: match e {
                FontInitError::InvalidEmbeddedFont { name, .. } => name,
            },
            style: match e {
                FontInitError::InvalidEmbeddedFont { .. } => "unknown".to_string(), // We don't have the style info here, so we return 'unknown'.
            },
            details: format!("font initialization failed: {:?}", e),
        }),
    }
}

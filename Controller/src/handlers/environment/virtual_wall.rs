use crate::handlers::environment::ensure_plotters_fonts;

use super::EnvironmentHandler;
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use plotters::prelude::*;
use serde_json::Value;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::info;
use virtual_wall::{
    StartOptions, TunnelDirection, TunnelEndpoint, TunnelRequest, VirtualWallManager,
};

#[derive(Clone)]
pub struct VirtualWallHandler {
    manager: Arc<Mutex<Option<Arc<VirtualWallManager>>>>,
}

impl VirtualWallHandler {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(Mutex::new(None)),
        }
    }

    async fn ensure_manager(&self) -> Result<Arc<VirtualWallManager>, String> {
        {
            let guard = self.manager.lock().await;
            if let Some(manager) = guard.as_ref() {
                return Ok(manager.clone());
            }
        }

        let mut guard = self.manager.lock().await;
        if let Some(manager) = guard.as_ref() {
            return Ok(manager.clone());
        }
        match VirtualWallManager::try_from_path(None) {
            Ok(manager) => {
                let manager = Arc::new(manager);
                *guard = Some(manager.clone());
                Ok(manager)
            }
            Err(err) => Err(format!("Failed to initialize Virtual Wall manager: {err}")),
        }
    }

    fn build_tunnel_request(params: HashMap<String, String>) -> Result<TunnelRequest, String> {
        let direction = match params
            .get("direction")
            .map(|s| s.as_str())
            .unwrap_or("remote")
        {
            "local" | "local-forward" => TunnelDirection::Local,
            "remote" | "remote-forward" => TunnelDirection::Remote,
            other => return Err(format!("Invalid `direction`: {other}")),
        };

        let node = params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        let listen_host = params
            .get("listen_host")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let listen_port = params
            .get("listen_port")
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| "Missing or invalid `listen_port` parameter".to_string())?;
        let target_host = params
            .get("target_host")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let target_port = params
            .get("target_port")
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| "Missing or invalid `target_port` parameter".to_string())?;

        Ok(TunnelRequest {
            node,
            direction,
            listen: TunnelEndpoint {
                host: listen_host,
                port: listen_port,
            },
            target: TunnelEndpoint {
                host: target_host,
                port: target_port,
            },
            username: params.get("username").cloned(),
        })
    }
}

#[async_trait]
impl EnvironmentHandler for VirtualWallHandler {
    async fn start(&self, options: &str) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        let parsed_options = StartOptions::from_query(options);
        info!(
            "Starting Virtual Wall environment with {} nodes (paths: {:?})",
            parsed_options.nodes, parsed_options.paths
        );
        match manager.start_from_options(parsed_options).await {
            Ok(summary) => Ok(format!(
                "Virtual Wall experiment `{}` started with {} resources",
                summary.experiment_name,
                summary.resources.len()
            )),
            Err(err) => Err(format!("Failed to start Virtual Wall: {err}")),
        }
    }

    async fn stop(&self) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        match manager.stop().await {
            Ok(_) => Ok("Virtual Wall environment stopped and resources released".to_string()),
            Err(err) => Err(format!("Failed to stop Virtual Wall: {err}")),
        }
    }

    async fn exec(&self, _params: HashMap<String, String>) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        let node = _params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        let command = _params
            .get("command")
            .cloned()
            .ok_or_else(|| "Missing `command` parameter".to_string())?;
        let username = _params.get("username").map(|s| s.as_str());

        let key_path = _params.get("identity_file").map(PathBuf::from);
        match manager
            .exec(&node, &command, username, key_path.as_deref(), None)
            .await
        {
            Ok(output) => Ok(output),
            Err(err) => Err(format!("Virtual Wall exec failed: {err}")),
        }
    }

    async fn nodes(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.nodes().await.map_err(|e| e.to_string())
    }

    async fn links(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.links().await.map_err(|e| e.to_string())
    }

    async fn status(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.status().await.map_err(|e| e.to_string())
    }

    async fn visualize(&self) -> Result<Vec<u8>, String> {
        let manager = self.ensure_manager().await?;
        let graph = manager.visualize().await.map_err(|e| e.to_string())?;
        render_graph_png(&graph)
    }

    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        let node = params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        manager
            .start_terminal(&node)
            .await
            .map_err(|e| format!("Failed to open shell: {e}"))
    }

    async fn ping_all(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.ping_all().await.map_err(|e| e.to_string())
    }

    async fn open_tunnel(&self, params: HashMap<String, String>) -> Result<Value, String> {
        let request = Self::build_tunnel_request(params)?;
        let manager = self.ensure_manager().await?;
        let tunnel = manager
            .open_tunnel(request)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(tunnel).map_err(|e| format!("Failed to serialize tunnel: {e}"))
    }

    async fn close_tunnel(&self, id: &str) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        manager
            .close_tunnel(id)
            .await
            .map_err(|e| format!("Failed to close tunnel {id}: {e}"))?;
        Ok(format!("Closed tunnel {id}"))
    }

    async fn list_tunnels(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        let tunnels = manager.list_tunnels().await.map_err(|e| e.to_string())?;
        serde_json::to_value(tunnels).map_err(|e| format!("Failed to serialize tunnels: {e}"))
    }
}

fn render_graph_png(graph: &Value) -> Result<Vec<u8>, String> {
    if let Err(e) = ensure_plotters_fonts() {
        tracing::info!("Plotters font init failed; visualization may be unavailable");
        tracing::error!("Plotters font init error: {e:?}");
    }

    let nodes = graph
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "visualize: missing nodes".to_string())?;
    let edges = graph
        .get("edges")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "visualize: missing edges".to_string())?;

    // Keep these as constants or wire them to config/env if you want.
    let width: u32 = 500;
    let height: u32 = 500;

    // plotters-bitmap expects a pre-sized RGB buffer: width * height * 3.
    let rgb_len = {
        let w = usize::try_from(width).map_err(|_| "visualize: width overflow".to_string())?;
        let h = usize::try_from(height).map_err(|_| "visualize: height overflow".to_string())?;
        w.checked_mul(h)
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| "visualize: image buffer size overflow".to_string())?
    };
    let mut rgb = vec![0u8; rgb_len];

    // Layout nodes in a circle.
    let count = nodes.len().max(1);
    let radius = 200.0;
    let center = (250.0, 250.0);
    let mut positions = HashMap::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let angle = (i as f64) * (2.0 * std::f64::consts::PI / count as f64);
        let x = (center.0 + radius * angle.cos()).round() as i32;
        let y = (center.1 + radius * angle.sin()).round() as i32;
        if let Some(id) = n.get("id").and_then(|v| v.as_str()) {
            positions.insert(id.to_string(), (x, y));
        }
    }

    {
        let root = BitMapBackend::with_buffer(&mut rgb, (width, height)).into_drawing_area();
        root.fill(&WHITE).map_err(|e| e.to_string())?;

        // Edges
        for e in edges {
            let src = e.get("src").and_then(|v| v.as_str());
            let dst = e.get("dst").and_then(|v| v.as_str());
            if let (Some(s), Some(d)) = (src, dst) {
                if let (Some(&p1), Some(&p2)) = (positions.get(s), positions.get(d)) {
                    let color = e.get("color").and_then(|c| c.as_str()).unwrap_or("#94a3b8");
                    let style = ShapeStyle::from(&parse_color(color)).stroke_width(2);
                    root.draw(&PathElement::new(vec![(p1.0, p1.1), (p2.0, p2.1)], style))
                        .map_err(|e| e.to_string())?;
                }
            }
        }

        // Nodes + labels
        for n in nodes {
            if let Some(id) = n.get("id").and_then(|v| v.as_str()) {
                if let Some(&(x, y)) = positions.get(id) {
                    let color = n.get("color").and_then(|c| c.as_str()).unwrap_or(
                        if n.get("type").and_then(|t| t.as_str()) == Some("switch") {
                            "#6b7280"
                        } else {
                            "#2563eb"
                        },
                    );
                    let style = ShapeStyle::from(&parse_color(color)).filled();
                    root.draw(&Circle::new((x, y), 8, style))
                        .map_err(|e| e.to_string())?;
                    root.draw(&Text::new(
                        id.to_string(),
                        (x + 10, y),
                        ("sans-serif", 12.0).into_font(),
                    ))
                    .map_err(|e| e.to_string())?;
                }
            }
        }

        root.present().map_err(|e| e.to_string())?;
    }

    // Encode RGB framebuffer -> PNG bytes
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgb, width, height, ColorType::Rgb8)
        .map_err(|e| format!("visualize: PNG encode failed: {e}"))?;

    // Base64 encode PNG bytes (this is what your frontend likely wants)
    Ok(BASE64.encode(&png).into_bytes())
}

fn parse_color(hex: &str) -> RGBColor {
    if let Some(stripped) = hex.strip_prefix('#') {
        if stripped.len() == 6 {
            if let Ok(rgb) = u32::from_str_radix(stripped, 16) {
                let r = ((rgb >> 16) & 0xFF) as u8;
                let g = ((rgb >> 8) & 0xFF) as u8;
                let b = (rgb & 0xFF) as u8;
                return RGBColor(r, g, b);
            }
        }
    }
    RGBColor(100, 100, 100)
}

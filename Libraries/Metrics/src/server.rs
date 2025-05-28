use axum::{http::StatusCode, routing::get, Router};
use tower_http::cors::CorsLayer;
use prometheus::{Encoder, TextEncoder};
use std::net::SocketAddr;

use crate::get_metrics;

/// Handler function for the /metrics endpoint.
pub async fn metrics_handler() -> Result<String, StatusCode> {
    let registry = {
        let metrics = get_metrics();
        metrics.registry().clone()
    };

    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    
    // Handle encoding errors gracefully
    if encoder.encode(&registry.gather(), &mut buffer).is_err() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Handle UTF-8 conversion errors gracefully
    match String::from_utf8(buffer) {
        Ok(metrics) => Ok(metrics),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Start an HTTP server to expose metrics.
pub async fn start_server(port: u16) {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        // Apply middleware
        .layer(
            // We allow cross-origin requests from any origin
            CorsLayer::permissive()
        );

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

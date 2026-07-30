mod config;
mod error;
mod http_util;
mod proxy;
mod stats;
mod storage;

use crate::config::{Config, LogLevel};
use crate::error::AppError;
use crate::proxy::AppState;
use crate::stats::Stats;
use crate::storage::{CacheManager, DiskCache, MemoryCache};

use axum::http::Request;
use axum::{
    routing::{any, get},
    Router,
};
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing::level_filters::LevelFilter;
use tracing::{info, warn};
use tracing_subscriber::Layer;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    let cfg = Config::from_env()?;
    cfg.validate()?;

    init_tracing(cfg.log_level);

    let stats = Arc::new(Stats::new());
    let mem = MemoryCache::new(&cfg);
    let disk = DiskCache::new(&cfg).await?;

    let cache = Arc::new(CacheManager::new(mem, disk));
    let origin = proxy::OriginClient::new(&cfg)?;

    let state = Arc::new(AppState::new(cfg.clone(), origin, cache, stats.clone()));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/stats", get(proxy::stats_handler))
        .route("/metrics", get(proxy::metrics_handler))
        // Socket.IO / Engine.IO must not go through the cache-only fallback.
        .route("/socket.io", any(proxy::socketio_handler))
        .route("/socket.io/", any(proxy::socketio_handler))
        .route("/socket.io/*path", any(proxy::socketio_handler))
        .fallback(proxy::proxy_handler)
        // Apply middleware
        .layer(
            // We allow cross-origin requests from any origin
            CorsLayer::permissive(),
        )
        .layer(
            // Add logging middleware
            ServiceBuilder::new().layer(
                TraceLayer::new_for_http()
                    .make_span_with(DefaultMakeSpan::new().include_headers(true))
                    .on_request(
                        |request: &Request<axum::body::Body>, _span: &tracing::Span| {
                            fn log_request(request: &Request<axum::body::Body>) {
                                let path = request.uri().path();
                                // If the path is /metrics, don't log it
                                if path == "/metrics" || path == "/dash/origin-time" {
                                    return;
                                }

                                if path.ends_with(".m4s")
                                    || path.ends_with(".part")
                                    || path.ends_with(".chunk")
                                    || path.ends_with(".mp4")
                                    || path.ends_with(".cmfv")
                                    || path.ends_with(".cmfa")
                                    || path.ends_with(".init")
                                    || path.ends_with(".mpd")
                                {
                                    return;
                                }

                                tracing::info!(
                                    "Received request for endpoint: {}",
                                    request.uri().path()
                                );
                            }
                            log_request(request);
                        },
                    ),
            ),
        )
        .with_state(state.clone());

    let cancel = CancellationToken::new();
    let sweep_cancel = cancel.clone();
    let sweep_stats = state.stats.clone();
    let sweep_cache = state.cache.clone();
    tokio::spawn(async move {
        if let Err(e) = sweep_cache
            .disk_sweeper_loop(sweep_cancel, sweep_stats)
            .await
        {
            warn!(error = %e, "disk sweeper loop ended with error");
        }
    });

    let addr: SocketAddr = cfg
        .listen_addr
        .parse()
        .map_err(|e| AppError::Config(format!("invalid listen addr '{}': {e}", cfg.listen_addr)))?;

    info!(%addr, "starting proxy");

    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel.cancel();
        info!("shutdown signal received");
    };

    // Optional inbound TLS termination.
    match (&cfg.tls_cert_pem, &cfg.tls_key_pem) {
        (Some(cert), Some(key)) => {
            info!(cert = %cert.display(), key = %key.display(), "serving HTTPS (rustls)");

            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();

            tokio::spawn(async move {
                shutdown.await;
                // pick a grace period you like
                shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10)));
            });

            axum_server::bind_rustls(
                addr,
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?,
            )
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .map_err(|e| AppError::Internal(format!("server error: {e}")))?;
        }
        (None, None) => {
            info!("serving HTTP (no TLS)");
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|e| AppError::Internal(format!("bind failed: {e}")))?;
            axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(shutdown)
                .await
                .map_err(|e| AppError::Internal(format!("server error: {e}")))?;
        }
        _ => {
            return Err(AppError::Config(
                "either provide both TLS cert+key or neither".to_string(),
            ));
        }
    }

    info!("stopped");
    Ok(())
}

fn init_tracing(log_level: LogLevel) {
    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(match log_level {
            LogLevel::Trace => LevelFilter::TRACE,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Error => LevelFilter::ERROR,
        });

    // Initialize console tracing if enabled
    #[cfg(feature = "console-tracing")]
    let subscriber = {
        let console_layer = console_subscriber::ConsoleLayer::builder()
            .retention(std::time::Duration::from_secs(60))
            .server_addr(([127, 0, 0, 1], 5556))
            .spawn();
        let tracy_layer = tracing_tracy::TracyLayer::default();
        tracing_subscriber::registry()
            .with(console_layer)
            .with(tracy_layer)
            .with(fmt_layer)
    };

    #[cfg(not(feature = "console-tracing"))]
    let subscriber = {
        use tracing_subscriber::layer::SubscriberExt;
        tracing_subscriber::registry().with(fmt_layer)
    };

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");
}

// main.rs

use axum::Router;
use clap::{Parser, ValueEnum};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder as HyperServerBuilder,
    service::TowerToHyperService,
};
use metrics::{get_all_interfaces, Metrics, MetricsBuilder, METRICS_UPDATE_PERIOD};
use rayon::ThreadPoolBuilder;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use shared_networking::tcp::{build_tcp_listener, TcpListenerOpts};
use shared_utils::crypto;
use std::{
    collections::HashMap,
    fs,
    io::{self, Cursor},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time,
};
use tokio::{runtime, sync::oneshot};
use tokio_rustls::{rustls, TlsAcceptor};
use tracing::{debug, error, info, instrument, level_filters::LevelFilter, warn};
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, Layer};
use url::Url;

mod decoders;
mod egress;
mod encoders;
mod generators;
mod handlers;
mod ingress;
mod processing;
mod router;
mod services;
#[cfg(test)]
mod test_support;
mod timing;
mod types;

use crate::types::{AdvertisedMoqConfig, MoqRelayRegistry};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, ValueEnum)]
enum LogLevel {
    Trace = 0, // Designates very fine-grained informational events, extremely verbose.
    Debug = 1, // Designates fine-grained informational events.
    Info = 2,  // Designates informational messages.
    Warn = 3,  // Designates hazardous situations.
    Error = 4, // Designates very serious errors.
}

#[derive(Parser, Debug)]
#[command(author, version, about = "pc-server")]
struct Args {
    // Set the port number
    #[arg(short, long, default_value = "3001")]
    port: u16,
    // Set the log level (possible values: error, warn, info, debug, trace)
    #[arg(short, long, default_value = "info")]
    log_level: LogLevel,
    /// Number of threads in the thread pool
    #[arg(short, long, default_value_t = 10)]
    threads: usize,
    /// FLUTE endpoint URL
    #[arg(long, default_value = "239.0.2.1")]
    flute_endpoint_url: String,
    /// FLUTE port
    #[arg(long, default_value_t = 40085)]
    flute_port: u16,
    /// URL of the MoQ relay (moqt:// or https://). If not provided, MoQ egress is disabled.
    /// Raw QUIC is used for moqt:// URLs, while web transport is used for https:// URLs.
    #[arg(long)]
    moq_url: Option<String>,
    /// Namespace announced in the MoQ catalog. Defaults to "multipathxr".
    #[arg(long, default_value = "/multipathxr")]
    moq_namespace: String,
    /// UDP bind address for the MoQ QUIC endpoint.
    #[arg(long, default_value = "[::]:0")]
    moq_bind: SocketAddr,
    /// Optional HTTPS port for the REST API.
    #[arg(long)]
    https_port: Option<u16>,
    /// PEM certificate(s) used by the MoQ publisher.
    #[arg(long = "moq-tls-cert")]
    moq_tls_cert: Vec<PathBuf>,
    /// PEM private key(s) used by the MoQ publisher.
    #[arg(long = "moq-tls-key")]
    moq_tls_key: Vec<PathBuf>,
    /// Additional root certificate(s) trusted by the MoQ publisher.
    #[arg(long = "moq-tls-root")]
    moq_tls_root: Vec<PathBuf>,
    /// Disable TLS verification when connecting to a remote MoQ relay.
    #[arg(long = "moq-tls-disable-verify", default_value_t = false)]
    moq_tls_disable_verify: bool,
}

#[instrument(skip_all)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let args = Args::parse();

    let base_log_level = match args.log_level {
        LogLevel::Trace => LevelFilter::TRACE,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Error => LevelFilter::ERROR,
    };

    // Keep our logs at the selected level, while muting noisy dependency targets.
    let log_targets = Targets::new()
        .with_default(base_log_level)
        .with_target("moq_transport", LevelFilter::OFF);

    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(log_targets);

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
    let subscriber = { tracing_subscriber::registry().with(fmt_layer) };

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");

    // WebRTC/DTLS (and MoQ) rely on Rustls; pick a deterministic backend upfront.
    crypto::install_default_crypto_provider();

    #[cfg(feature = "console-tracing")]
    info!("Console tracing enabled.");

    info!("{:?}", args);
    let server_instance_id = Arc::new(uuid::Uuid::new_v4().to_string());
    info!("Server instance id: {}", server_instance_id);

    let runtime = runtime::Builder::new_multi_thread()
        //.worker_threads(2)
        .thread_name_fn(|| {
            static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0);
            let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("MAIN_R w-{id}")
        })
        .enable_all()
        .build()
        .unwrap();

    // Initialize thread pool
    let thread_pool = Arc::new(
        ThreadPoolBuilder::new()
            .thread_name(|i| format!("TP w-{}", i + 1))
            .num_threads(args.threads)
            .build()
            .expect("Failed to build thread pool"),
    );

    // Thread-safe storage for active jobs
    let active_jobs = Arc::new(tokio::sync::RwLock::new(HashMap::<
        String,
        oneshot::Sender<()>,
    >::new()));

    // Retrieve all network interfaces
    let interfaces = get_all_interfaces();
    if interfaces.is_empty() {
        error!("No network interfaces found to track.");
        return Err("No network interfaces available.".into());
    }
    info!("Tracking the following interfaces: {:?}", interfaces);

    // Build the metrics instance, tracking all interfaces
    let mut builder = MetricsBuilder::new().add_label("mode", "server");

    for interface in interfaces {
        builder = builder.track_interface(&interface);
    }

    let metrics = builder.build();

    // Start the metrics update loop
    // These are for some default system metrics
    // We are responsible for updating your custom metrics
    let metrics_clone = Arc::new(metrics);
    runtime.spawn(update_metrics_loop(metrics_clone));

    // Initialize services
    let stream_manager = Arc::new(services::stream_manager::StreamManager::new());
    let mut mpd_manager = services::mpd_manager::MpdManager::new();
    let processing_pipeline = Arc::new(processing::ProcessingPipeline::new(thread_pool.clone()));

    let moq_registry = Arc::new(MoqRelayRegistry::default());

    let (moq_config, moq_advertised_config) = if let Some(url) = &args.moq_url {
        let parsed_url = Url::parse(url).inspect_err(|&err| {
            error!("Invalid MoQ URL provided: {}", err);
        })?;
        let advertised = AdvertisedMoqConfig {
            url: parsed_url.to_string(),
            namespace: args.moq_namespace.clone(),
            tls_ca_pem: args
                .moq_tls_root
                .first()
                .and_then(|path| match fs::read_to_string(path) {
                    Ok(contents) => Some(contents),
                    Err(err) => {
                        warn!(
                            "Failed to read MoQ TLS root '{}' for advertisement: {err}",
                            path.display()
                        );
                        None
                    }
                }),
        };
        let _compatibility_options = (
            args.moq_bind,
            &args.moq_tls_cert,
            &args.moq_tls_key,
            args.moq_tls_disable_verify,
        );

        (
            Some(egress::moq::MoqEgressConfig {
                url: parsed_url,
                namespace: args.moq_namespace.clone(),
            }),
            Some(advertised),
        )
    } else {
        (None, None)
    };

    // Add signalling callback to the MPD manager
    let stream_manager_clone = stream_manager.clone();
    let callback = {
        let stream_manager_clone = stream_manager_clone.clone();
        runtime.block_on(async move {
            let local_runtime = tokio::runtime::Handle::current();
            Arc::new(move |group_id: String| {
                let stream_manager_clone = stream_manager_clone.clone();
                local_runtime.spawn(async move {
                    if let Some(io) = stream_manager_clone.get_socket_io() {
                        let _ = io.emit("mpd::group_id", &group_id).await;
                    } else {
                        error!("Socket IO is not initialized");
                    }
                });
            })
        })
    };

    mpd_manager.set_notify_callback(callback);
    // Wrap the MPD manager in an Arc
    let mpd_manager = Arc::new(mpd_manager);

    // Initialize singleton egress protocols
    egress::initialize_egress_protocols(
        stream_manager.clone(),
        mpd_manager.clone(),
        processing_pipeline.clone(),
        args.flute_endpoint_url.clone(),
        args.flute_port,
        moq_config,
        server_instance_id.clone(),
    );

    // Initialize singleton ingress protocols
    ingress::initialize_ingress_protocols(stream_manager.clone(), processing_pipeline.clone());

    // Create router
    let app = router::create_router(
        stream_manager.clone(),
        processing_pipeline.clone(),
        active_jobs.clone().into(),
        moq_advertised_config,
        moq_registry.clone(),
        server_instance_id.clone(),
    );
    let https_app = app.clone();

    runtime.block_on(async move {
        let http_addr: SocketAddr = format!("0.0.0.0:{}", args.port).parse().unwrap();
        let http_server = serve_http(app, http_addr);

        if let Some(https_port) = args.https_port {
            let cert_path = args.moq_tls_cert.first().cloned();
            let key_path = args.moq_tls_key.first().cloned();
            if cert_path.is_none() || key_path.is_none() {
                error!("HTTPS port specified but missing --moq-tls-cert/--moq-tls-key");
                http_server.await;
                return;
            }

            let https_addr: SocketAddr = format!("0.0.0.0:{https_port}")
                .parse()
                .expect("invalid https port");
            info!(
                "Starting HTTPS server on {} with cert={} key={}",
                https_addr,
                cert_path.as_ref().unwrap().display(),
                key_path.as_ref().unwrap().display()
            );
            let https_server =
                serve_https(https_app, https_addr, cert_path.unwrap(), key_path.unwrap());

            tokio::select! {
                _ = http_server => {},
                _ = https_server => {},
            };
        } else {
            http_server.await;
        }
    });

    info!("Server started");

    loop {
        std::thread::sleep(time::Duration::from_secs(1));
    }

    #[allow(unreachable_code)]
    Ok(())
}

async fn serve_http(app: Router, addr: SocketAddr) {
    let std_listener = match build_tcp_listener(TcpListenerOpts {
        addr,
        backlog: 1024,
        reuse_port: true,
        nonblocking: true,
        nodelay: true,
    }) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to build HTTP listener: {}", e);
            return;
        }
    };

    let listener = match tokio::net::TcpListener::from_std(std_listener) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to convert HTTP listener: {}", e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        error!("HTTP server error: {}", e);
    }
}

async fn serve_https(app: Router, addr: SocketAddr, cert: PathBuf, key: PathBuf) {
    let tls_config = match build_https_config(cert, key) {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to build HTTPS TLS config: {}", e);
            return;
        }
    };

    let std_listener = match build_tcp_listener(TcpListenerOpts {
        addr,
        backlog: 1024,
        reuse_port: true,
        nonblocking: true,
        nodelay: true,
    }) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to build HTTPS listener: {}", e);
            return;
        }
    };

    let listener = match tokio::net::TcpListener::from_std(std_listener) {
        Ok(listener) => listener,
        Err(e) => {
            error!("Failed to convert HTTPS listener: {}", e);
            return;
        }
    };

    let acceptor = TlsAcceptor::from(tls_config);
    let router = app;

    info!("HTTPS listener active on {}", addr);

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let acceptor = acceptor.clone();
                let service = TowerToHyperService::new(router.clone());
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            let builder = HyperServerBuilder::new(TokioExecutor::new());
                            if let Err(e) = builder
                                .serve_connection(TokioIo::new(tls_stream), service)
                                .await
                            {
                                warn!("HTTPS connection error from {}: {}", peer_addr, e);
                            }
                        }
                        Err(e) => {
                            // The local agent metrics scanner discovers listening TCP ports and
                            // probes them with plain HTTP GET /metrics requests. When one of those
                            // probes lands on this HTTPS listener, rustls rejects it as an invalid
                            // TLS content type. That warning is expected and safe to ignore unless
                            // the peer was actually supposed to speak TLS.
                            warn!("TLS handshake failed from {}: {}", peer_addr, e)
                        }
                    }
                });
            }
            Err(e) => {
                warn!("HTTPS accept error: {}", e);
            }
        }
    }
}

fn build_https_config(
    cert_path: PathBuf,
    key_path: PathBuf,
) -> Result<Arc<rustls::ServerConfig>, Box<dyn std::error::Error>> {
    info!(
        "Loading HTTPS TLS materials cert={} key={}",
        cert_path.display(),
        key_path.display()
    );
    let cert_bytes = fs::read(cert_path)?;
    let mut cert_reader = Cursor::new(cert_bytes);
    let cert_chain: Vec<CertificateDer<'static>> =
        certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;
    if cert_chain.is_empty() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTPS certificate file contains no certificates",
        )));
    }

    let key_bytes = fs::read(key_path)?;
    let mut key_reader = Cursor::new(&key_bytes[..]);
    let pkcs8_keys: Vec<PrivateKeyDer<'static>> = pkcs8_private_keys(&mut key_reader)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(Into::into)
        .collect();

    let private_key = if let Some(key) = pkcs8_keys.into_iter().next() {
        key
    } else {
        let mut rsa_reader = Cursor::new(&key_bytes[..]);
        let rsa_keys: Vec<PrivateKeyDer<'static>> = rsa_private_keys(&mut rsa_reader)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(Into::into)
            .collect();
        rsa_keys.into_iter().next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "HTTPS private key file contains no keys",
            )
        })?
    };

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)?;
    Ok(Arc::new(config))
}

#[instrument(skip_all)]
async fn update_metrics_loop(metrics: Arc<Metrics>) {
    // Tunables
    const CATCHUP_FRACTION: f64 = 0.75; // shave up to 75% to catch up
    let period = METRICS_UPDATE_PERIOD;
    let skip_threshold = period.mul_f64(0.95);

    // Anchor to a fixed grid
    let start = tokio::time::Instant::now();
    let mut tick_idx: u64 = 1;

    loop {
        // Do the work for this tick
        metrics.update();
        debug!("Metrics updated");

        // ---- Drift-resistant timing with bounded catch-up ----
        let now = tokio::time::Instant::now();
        let target = start + period.saturating_mul(tick_idx as u32);

        if now < target {
            // Early: sleep exactly to the grid time
            tokio::time::sleep_until(target).await;
            tick_idx += 1;
            continue;
        }

        // We're late relative to the grid
        let lateness = now.saturating_duration_since(target);

        if lateness < skip_threshold {
            // Prefer not to skip: shorten next sleep (bounded)
            let catchup_cap = period.mul_f64(CATCHUP_FRACTION);
            let shave = if lateness > catchup_cap {
                catchup_cap
            } else {
                lateness
            };
            let sleep_dur = period.saturating_sub(shave);

            if !sleep_dur.is_zero() {
                tokio::time::sleep(sleep_dur).await;
            }
            tick_idx += 1;
        } else {
            // Very late (~full second): snap to current grid slot (single skip)
            let elapsed = now.duration_since(start);
            let full_ticks = (elapsed.as_nanos() / period.as_nanos()) as u64;
            tick_idx = full_ticks + 1;
            // no sleep; loop again
        }
    }
}

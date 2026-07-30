use pc_receiver::{
    args::{get_log_level_filter, parse_args},
    ingress::Ingress,
    services::stream_manager::{MoqClientConfig, MoqTlsConfig},
    utils::{create_metrics, start_metrics_server},
};
use shared_utils::crypto;
use std::time::{Duration, Instant};
use tracing::level_filters::LevelFilter;
use tracing::{debug, error, info};
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, Layer};
use url::Url;

fn main() {
    let args = parse_args();

    let base_log_level = get_log_level_filter(&args);
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
            .retention(Duration::from_secs(60))
            .server_addr(([127, 0, 0, 1], 5555))
            .spawn();
        tracing_subscriber::registry()
            .with(console_layer)
            .with(fmt_layer)
    };

    #[cfg(not(feature = "console-tracing"))]
    let subscriber = { tracing_subscriber::registry().with(fmt_layer) };

    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global default subscriber");

    // Avoid runtime rustls panics by picking our crypto provider immediately.
    crypto::install_default_crypto_provider();

    info!("Starting receiver client (headless)");
    info!("{:?}", args);

    create_metrics().unwrap();

    // Initialize the ingress system
    let ingress = Ingress::new(10, args.disable_parser);
    // Set the parameters first before initializing
    {
        let stream_manager = ingress.get_stream_manager();
        stream_manager.set_http_url(args.http_url);
        stream_manager.set_websocket_url(args.websocket_url);
        stream_manager.set_flute_url(args.multicast_url);
        if let Some(moq_url) = args.moq_url.as_ref() {
            match Url::parse(moq_url) {
                Ok(url) => {
                    let tls_args = MoqTlsConfig {
                        cert: args.moq_tls_cert.clone(),
                        key: args.moq_tls_key.clone(),
                        root: args.moq_tls_root.clone(),
                        disable_verify: args.moq_tls_disable_verify,
                    };
                    stream_manager.set_moq_config(MoqClientConfig {
                        url,
                        namespace: args.moq_namespace.clone(),
                        bind: args.moq_bind,
                        tls: tls_args,
                    });
                }
                Err(err) => error!("Invalid MoQ URL provided: {err}"),
            }
        }
    }
    // Finish initializing the ingress system
    ingress.initialize();

    start_metrics_server(args.port);

    info!("Receiver client initialized");

    // Get the storage
    let storage = ingress.get_storage();

    let fps = 30;
    let frame_period = frame_offset_duration(1, fps);
    let schedule_start = Instant::now();
    let mut next_frame_index = 0_u64;
    // A backlog threshold where we decide to skip older frames
    let skip_threshold = 10; // number of frames in the queue
    loop {
        let now = Instant::now();
        let mut frame_target = schedule_start + frame_offset_duration(next_frame_index, fps);
        if now >= frame_target && now.duration_since(frame_target) >= frame_period {
            let current_grid_index =
                frame_index_for_elapsed(now.duration_since(schedule_start), fps);
            if current_grid_index > next_frame_index {
                debug!(
                    "Frame consumption loop late by {:?}; snapping from tick {} to {}.",
                    now.duration_since(frame_target),
                    next_frame_index,
                    current_grid_index
                );
                next_frame_index = current_grid_index;
                frame_target = schedule_start + frame_offset_duration(next_frame_index, fps);
            }
        }

        let wait_now = Instant::now();
        if wait_now < frame_target {
            std::thread::sleep(frame_target - wait_now);
        }
        next_frame_index = next_frame_index.saturating_add(1);

        let start = Instant::now();
        // Get all the stream ids in the storage
        let stream_ids = storage.get_stream_ids();
        // For each stream id, consume a frame
        for stream_id in stream_ids {
            let frames_in_buffer = storage.get_frame_count(&stream_id);
            // If backlog is too large, skip older frames
            if frames_in_buffer > skip_threshold {
                let frames_to_skip = frames_in_buffer.saturating_sub(1);
                // e.g., skip all but the very last frame
                let removed = storage.remove_oldest_frames(&stream_id, frames_to_skip);
                if removed > 0 {
                    info!(
                        "Skipped {} oldest frames for stream_id = {} (too large backlog).",
                        removed, stream_id
                    );
                }
            }

            // Get the frame data
            let frame_data = storage.consume_frame(&stream_id);
            if let Some(frame_data) = frame_data {
                // Process the frame data
                debug!(
                    "Consumed frame data for stream id: {} with {} points",
                    stream_id, frame_data.point_count
                );
            }
        }

        // Check the backlog (maximum number of frames in any stream)
        let highest_frame_count = storage.get_highest_frame_count();
        storage.current_backlog.set(highest_frame_count as i64);

        let elapsed = start.elapsed();
        if elapsed > frame_period {
            error!(
                "Frame consumption took longer than the target frame period by {:?}.",
                elapsed - frame_period
            );
        }
    }
}

fn frame_offset_duration(frame_index: u64, fps: u32) -> Duration {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let fps = fps.max(1) as u128;
    let nanos = NANOS_PER_SECOND.saturating_mul(frame_index as u128) / fps;
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

fn frame_index_for_elapsed(elapsed: Duration, fps: u32) -> u64 {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let fps = fps.max(1) as u128;
    let index = elapsed.as_nanos().saturating_mul(fps) / NANOS_PER_SECOND;
    index.min(u64::MAX as u128) as u64
}

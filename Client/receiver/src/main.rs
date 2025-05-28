use pc_receiver::{args::{get_log_level_filter, parse_args}, ingress::Ingress, utils::{create_metrics, start_metrics_server}};
use tracing::{debug, error, info};
use tracing_subscriber::{layer::SubscriberExt, Layer};
use std::time::Duration;

fn main() {
    let args = parse_args();

    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(get_log_level_filter(&args));

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
    let subscriber = {
        tracing_subscriber::registry()
            .with(fmt_layer)
    };

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global default subscriber");

    info!("Starting receiver client (headless)");
    info!("{:?}", args);


    create_metrics().unwrap();

    // Initialize the ingress system
    let ingress = Ingress::new(10, args.disable_parser);
    // Set the parameters first before initializing
    let stream_manager = ingress.get_stream_manager();
    stream_manager.set_websocket_url(args.server_url);
    stream_manager.set_flute_url(args.multicast_url);
    // Finish initializing the ingress system
    ingress.initialize();

    start_metrics_server(args.port);

    info!("Receiver client initialized");

    // Get the storage
    let storage = ingress.get_storage();

    // For demonstration, loop forever at 30 frames per second
    let fps = 30;
    let max_wait_time = std::time::Duration::from_secs_f32(1.0 / fps as f32);
    // A backlog threshold where we decide to skip older frames
    let skip_threshold = 10; // number of frames in the queue
    // A backlog threshold where we *start* adjusting wait times
    let catchup_threshold = 3;
    loop {
        let start = std::time::Instant::now();
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
                storage.frames_skipped_total.add(removed as i64);
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
                info!("Consumed frame data for stream id: {} with {} points", stream_id, frame_data.point_count);
            }
        }

        // Check the backlog (maximum number of frames in any stream)
        let highest_frame_count = storage.get_highest_frame_count();
        storage.current_backlog.set(highest_frame_count as i64);

        // If the backlog is beyond a certain threshold, we accelerate consumption
        // by reducing the sleep time. You could also consume multiple frames
        // from each stream each loop iteration, or do any other catch-up strategy.
        let dynamic_frame_duration = if highest_frame_count >= catchup_threshold {
            // For example, cut the sleep time proportionally to backlog
            // The higher the backlog, the more we reduce the wait
            let factor = (highest_frame_count - catchup_threshold + 1) as f32;
            // This factor can be computed in various ways:
            //   - linear
            //   - exponential
            //   - step-based
            // Example: half the normal wait time for each backlog count above threshold.
            // Feel free to tune or clamp this as needed.
            let adjusted = max_wait_time.div_f32(2_f32.powf(factor.min(5.0)));
            debug!(
                "Backlog = {}, reducing wait time from {:?} to {:?}.",
                highest_frame_count, max_wait_time, adjusted
            );
            adjusted
        } else {
            // If backlog is not too large, use normal wait time
            max_wait_time
        };

        // Wait for the remaining time      
        let elapsed = start.elapsed();
        if elapsed < dynamic_frame_duration {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            error!("Frame consumption took longer than the target wait time.");
        }
    }
}
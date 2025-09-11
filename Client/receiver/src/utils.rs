use std::{sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex}};

use metrics::{get_all_interfaces, MetricsBuilder, start_server_graceful};
use tokio::{runtime::Builder, sync::oneshot, time::{Duration, Instant}};
use tracing::{debug, error, info};
use once_cell::sync::Lazy;

/// Handle that lets us shut down a running metrics server thread.
struct ServerControl {
    shutdown: Option<oneshot::Sender<()>>,     // signal channel to ask the server to stop
    handle:   Option<std::thread::JoinHandle<()>>, // underlying OS thread running the Tokio runtime server
    update_thread: Option<std::thread::JoinHandle<()>>,  // underlying OS thread running the metrics update loop
}

/// Global guard so only **one** metrics-export HTTP server can be alive at a time.
/// If [`start_metrics_server`] is called again, the existing server is stopped first.
static METRICS_SERVER: Lazy<Mutex<Option<ServerControl>>> = Lazy::new(|| Mutex::new(None));
// Global guard to track if the metrics update loop is running.
static METRICS_RUNNING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));



pub fn create_metrics() -> Result<(), Box<dyn std::error::Error>> {
    // Retrieve all network interfaces
    let interfaces = get_all_interfaces();
    if interfaces.is_empty() {
        error!("No network interfaces found to track.");
        return Err("No network interfaces available.".into());
    }
    info!("Tracking the following interfaces: {:?}", interfaces);

    METRICS_RUNNING.store(true, Ordering::SeqCst);
    
    // Build the metrics instance, tracking all interfaces
    let mut builder = MetricsBuilder::new().add_label("mode", "client");

    for interface in interfaces {
        builder = builder.track_interface(&interface);
    }

    let metrics = builder.build();

    // Start the metrics update loop
    // These are for some default system metrics
    // We are responsible for updating your custom metrics
    let metrics_clone = Arc::new(metrics);
    let handle = match std::thread::Builder::new()
        .name("metrics-update".into())
        .spawn(move || {
            const PERIOD: Duration = Duration::from_secs(1);
            const CATCHUP_FRACTION: f64 = 0.75;                 // shave up to 75% to catch up
            const SKIP_THRESHOLD: Duration = Duration::from_millis(950); // only skip if ≥ 0.95 s late

            // Anchor to a fixed monotonic grid: t = start + n * PERIOD
            let start = Instant::now();
            let mut tick_idx: u64 = 1;

            while METRICS_RUNNING.load(Ordering::SeqCst) {
                // Do the work for this tick
                metrics_clone.update();
                debug!("Metrics updated");

                // ---- Drift-resistant timing with bounded catch-up ----
                let now = Instant::now();
                let target = start + PERIOD.saturating_mul(tick_idx as u32);

                if now < target {
                    // Early: sleep exactly until the grid time
                    std::thread::sleep(target - now);
                    tick_idx += 1;
                    continue;
                }

                // Late relative to the grid
                let lateness = now.duration_since(target);

                if lateness < SKIP_THRESHOLD {
                    // Prefer not to skip: shorten next sleep by min(lateness, catchup_cap)
                    let catchup_cap_ns =
                        (PERIOD.as_nanos() as f64 * CATCHUP_FRACTION) as u128;
                    let catchup_cap = Duration::from_nanos(catchup_cap_ns as u64);

                    let shave = if lateness > catchup_cap { catchup_cap } else { lateness };
                    let sleep_dur = PERIOD.saturating_sub(shave);
                    if !sleep_dur.is_zero() {
                        std::thread::sleep(sleep_dur);
                    }
                    tick_idx += 1;
                } else {
                    // Very late (~full second or more): snap to current grid slot (single skip)
                    let elapsed = now.duration_since(start);
                    let full_ticks = (elapsed.as_nanos() / PERIOD.as_nanos()) as u64;
                    tick_idx = full_ticks + 1;
                    // no sleep; immediately loop again
                }
            }

            info!("Metrics update thread stopped");
        }
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to spawn metrics server thread: {}", e);
            return Err("Failed to spawn metrics update thread".into());
        }
    };

    // Store the handle in the global state
    let mut guard = METRICS_SERVER.lock().expect("metrics server mutex poisoned");
    let entry = guard.get_or_insert(ServerControl {
        shutdown: None,
        handle:   None,
        update_thread: None,
    });
    entry.update_thread = Some(handle); // TODO: check why the handle can't be found when stopping the server

    Ok(())
}

pub fn start_metrics_server(port: u16) {
    // ────────────────────────────────────────────────────────────────────────────
    // 1.  Shut down any existing instance, if present.
    // Keep the update thread if it's still running
    // ────────────────────────────────────────────────────────────────────────────
    let mut preserved_update_thread: Option<std::thread::JoinHandle<()>> = None;
    {
        let mut guard = METRICS_SERVER.lock().expect("metrics server mutex poisoned");
        if let Some(mut control) = guard.take() {
            // Preserve the update thread handle (do NOT drop it!)
            preserved_update_thread = control.update_thread.take();
            if let Some(tx) = control.shutdown.take() {
                let _ = tx.send(());                           // ask the running server to exit
            }
            if let Some(handle) = control.handle.take() {
                let _ = handle.join();                          // wait for the thread to finish
                info!("Previous metrics server stopped");
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────────────
    // 2.  Prepare a shutdown channel for the new instance.
    // ────────────────────────────────────────────────────────────────────────────
    let (tx, rx) = oneshot::channel::<()>();

    // ────────────────────────────────────────────────────────────────────────────
    // 3.  Spawn the server on its own OS thread with a dedicated Tokio runtime.
    // ────────────────────────────────────────────────────────────────────────────
    let handle = match std::thread::Builder::new()
        .name("metrics-server".into())
        .spawn(move || {
            // Inside this thread, create a runtime
            let runtime = 
                Builder::new_multi_thread()
                    .thread_name_fn(|| {
                        static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        format!("MTRC_R w-{id}")
                    })
                    .enable_all()
                    .build()
                    .expect("Failed to build runtime");

            // Now, run the server from the runtime
            runtime.block_on(async {
                start_server_graceful(port, rx).await;
            });

            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
            info!("Metrics server stopped");
        }
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to spawn metrics server thread: {}", e);
            return;
        }
    };

    // ────────────────────────────────────────────────────────────────────────────
    // 4.  Store the control handle so we can shut it down next time.
    // ────────────────────────────────────────────────────────────────────────────
    let mut guard = METRICS_SERVER.lock().expect("metrics server mutex poisoned");
    let entry = guard.get_or_insert(ServerControl {
        shutdown: None,
        handle:   None,
        update_thread: preserved_update_thread,
    });
    entry.shutdown = Some(tx);
    entry.handle = Some(handle);
}

/// Try to shut the exporter down gracefully and, if it does not
/// terminate within `timeout`, force the entire process to abort.
///
/// Returns `Ok(())` when the server stopped cleanly, otherwise
/// returns an `Err` explaining what happened just before the force-kill.
pub fn stop_metrics_server() -> Result<(), &'static str> {
    //use std::process;

    // Notify the update thread that we are shutting down.
    METRICS_RUNNING.store(false, Ordering::SeqCst);

    let timeout = std::time::Duration::from_secs(1);

    // Grab the singleton guard.
    let mut guard = METRICS_SERVER
        .lock()
        .map_err(|_| "server-control mutex poisoned")?;

    // Nothing running?
    let Some(ServerControl {
        mut shutdown,
        mut handle,
        mut update_thread,
    }) = guard.take()
    else {
        info!("No metrics started, nothing to stop");
        return Ok(()); // nothing to stop
    };

    // ---- 1.  Ask to stop the metrics server and update threads. ---------------------------------------
    if let Some(tx) = shutdown.take() {
        let _ = tx.send(());                // ignore if already gone
    }
    info!("Asked metrics server and update thread to stop");

    // ---- 2.  If we had an update thread, we will wait for it to stop.
    if let Some(update_thread) = update_thread.take() {
        let _ = update_thread.join(); // ignore panic payload
        // If the update thread panics, we just log it and continue.
        info!("Metrics update thread stopped");
    } else {
        info!("No metrics update thread to stop");
    }

    // ---- 3.  Wait for it the server finish (with a timeout). ------------
    if let Some(handle) = handle.take() {
        // Because `join()` blocks, off-load the join to a helper.
        let (done_tx, done_rx) = mpsc::channel();
        let temp_handle = std::thread::Builder::new()
            .name("metrics_server_join_thread".to_string())
            .spawn(move || {
            let _ = handle.join();          // ignore panic payload
            let _ = done_tx.send(());       // ignore send errors
        }).expect("Failed to spawn metrics server join thread");

        info!("Waiting for metrics server to stop...");

        // Did we shut down in time?
        if done_rx.recv_timeout(timeout).is_ok() {
            info!("Metrics server stopped cleanly");
            //TODO: should we close the channel?
            return Ok(());
        }



        // Wait for the helper thread to finish.
        let _ = temp_handle.join(); // ignore panic payload

        // ---- 4.  Force-kill. ------------------------------------
        error!("metrics server did not shut down in {timeout:?}");
        //process::abort();                   // never returns
    }

    // (#-[allow(unreachable_code)] for completeness)
    Err("unexpected: no thread handle")
}

#[macro_export]
macro_rules! log_drop {
    ($t:ty) => {
        impl ::std::ops::Drop for $t {
            fn drop(&mut self) {
                ::tracing::info!("drop → {}", ::std::any::type_name::<$t>());
            }
        }
    };
}
//! examples/debug_main.rs
//! Tiny CLI for debugging -- **no external crates**.

use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::{c_char, CStr},
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use interoptopus::patterns::{
    slice::FFISlice,
    string::AsciiPointer,
};
use pc_receiver::ffi::{
    consume_frame, destroy, get_stream_ids, ingress_subscribe, ingress_unsubscribe, init,
    register_debug_callback, unregister_debug_callback, DebugCallback, SubscriptionCallback,
};

const FPS: u64 = 30;
const POLL_INTERVAL_MS: u64 = 500;

/* ───── helper ───── */

#[inline]
fn slice_from_bytes(bytes: &[u8]) -> FFISlice<'_, u8> {
    bytes.into()
}

/* ───── live stats per stream ───── */

#[derive(Default, Clone)]
struct Stats {
    last_pts:  u64,
    points:    u64,          // last frame’s point-count
    frames:    u64,          // frames seen in current window
}
type StatsMap = Arc<Mutex<HashMap<String, Stats>>>;

/* ───── shared state ───── */

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

type StreamSet = Arc<Mutex<HashSet<String>>>;
static STREAMS: once_cell::sync::Lazy<StreamSet> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashSet::new())));

static STATS: once_cell::sync::Lazy<StatsMap> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/* ───── callbacks coming from the Rust library ───── */

extern "C" fn debug_cb(msg: AsciiPointer, level: AsciiPointer) {
    println!(
        "[rust {:>5}] {}",
        level.as_str().unwrap_or("?"),
        msg.as_str().unwrap_or("")
    );
}

extern "C" fn stream_list_cb(csv_ptr: *const c_char) {
    if csv_ptr.is_null() {
        return;
    }
    let csv = unsafe { CStr::from_ptr(csv_ptr) }.to_string_lossy();
    let mut set = STREAMS.lock().unwrap();
    set.clear();
    for id in csv.split(',').filter(|s| !s.is_empty()) {
        set.insert(id.to_owned());
    }
}

extern "C" fn frame_cb(
    _send_time: u64,
    presentation_time: u64,
    error_count: u64,
    point_count: u64,
    _coords: FFISlice<f32>,
    _colors: FFISlice<u8>,
    stream_id: AsciiPointer,
) {
    let id = stream_id.as_str().unwrap_or("?").to_owned();

    // ❶ accumulate per-stream stats
    let mut map = STATS.lock().unwrap();
    let entry = map.entry(id).or_default();
    entry.last_pts = presentation_time;
    entry.points   = point_count;
    entry.frames  += 1;

    // ❷ still print decoding errors immediately
    if error_count > 0 {
        println!(
            "⚠️  stream={} pts={} err={} points={}",
            stream_id.as_str().unwrap_or("?"),
            presentation_time,
            error_count,
            point_count
        );
    }
}

/* ───── thread dump ───── */

#[cfg(target_os = "linux")]
fn dump_threads() -> std::io::Result<()> {
    use std::{fs, io::Read, path::PathBuf};

    println!("\n───────── live threads ─────────");
    println!("{:<8} │ {:<20} │ {}", "TID", "STATE", "NAME");
    println!("─────────┼──────────────────────┼───────────────────────────────");

    // Every sub-directory of /proc/self/task is a thread-id (TID)
    for entry in fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let tid = entry.file_name().into_string().unwrap_or_default();

        let mut status_path = PathBuf::from("/proc/self/task");
        status_path.push(&tid);
        status_path.push("status");

        // Parse the `Name:` and `State:` lines from /proc/.../status
        let mut contents = String::new();
        fs::File::open(&status_path)?.read_to_string(&mut contents)?;
        let mut name = "<unknown>".to_string();
        let mut state = "<unknown>".to_string();
        for line in contents.lines() {
            if line.starts_with("Name:")   { name  = line[5..].trim().to_owned(); }
            if line.starts_with("State:")  { state = line[6..].trim().to_owned(); }
            if name != "<unknown>" && state != "<unknown>" { break; }
        }

        println!("{:<8} │ {:<20} │ {}", tid, state, name);
    }
    println!("──────────────────────────────────────────────────────────────\n");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn dump_threads() -> std::io::Result<()> {
    // Fallback: nothing to do on non-Linux – feel free to add a Windows / macOS impl.
    Ok(())
}

/* ───── main ───── */

fn main() -> Result<(), Box<dyn std::error::Error>> {
    /* ─── tiny CLI ─── */
    let mut server = "http://localhost:3001".to_string();
    let mut mcast = "udp://239.0.0.1:40085".to_string();
    let mut level = 2u32;

    let mut it = env::args();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--server-url" => server = it.next().unwrap_or(server),
            "--multicast-url" => mcast = it.next().unwrap_or(mcast),
            "--log-level" => level = it.next().and_then(|s| s.parse().ok()).unwrap_or(level),
            _ => {}
        }
    }

    println!("🟢  server     {server}");
    println!("🟢  multicast  {mcast}");
    println!("🟢  log-level  {level}");

    /* ─── register FFI callbacks ─── */
    register_debug_callback(DebugCallback::new(debug_cb));
    ingress_subscribe(SubscriptionCallback::new(frame_cb));

    /* ─── start the receiver ─── */
    let server_c = std::ffi::CString::new(server)?;
    let mcast_c  = std::ffi::CString::new(mcast )?;
    init(
        level,
        AsciiPointer::from_cstr(&server_c),
        AsciiPointer::from_cstr(&mcast_c),
    );

    /* ─── thread: poll stream list ─── */
    let poller = thread::Builder::new()
        .name("poll_stream_list".to_string())
        .spawn(|| {
            while !SHUTTING_DOWN.load(Ordering::SeqCst) {
                get_stream_ids(stream_list_cb);
                thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
            }
            println!("🛑  stream list poller stopped");
        })?;

    /* ─── thread: pull frames ─── */
    let consumer = thread::Builder::new()
        .name("frame_consumer".to_string())
        .spawn(|| {
            let frame_dt = Duration::from_millis(1_000 / FPS);
            while !SHUTTING_DOWN.load(Ordering::SeqCst) {
                let start = Instant::now();
                for id in STREAMS.lock().unwrap().clone() {
                    consume_frame(slice_from_bytes(id.as_bytes()));
                }
                let elapsed = start.elapsed();
                if elapsed < frame_dt {
                    thread::sleep(frame_dt - elapsed);
                }
            }
            println!("🛑  frame consumer stopped");
        })?;

    /* ─── thread: live dashboard ─── */
    let dashboard = thread::Builder::new()
        .name("live_dashboard".to_string())
        .spawn(|| {
            let mut last_refresh = Instant::now();
            let mut last_counts  = HashMap::<String, u64>::new();

            while !SHUTTING_DOWN.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(250));

                if last_refresh.elapsed() >= Duration::from_secs(1) {
                    // ── gather & compute ────────────────────────────
                    let stats = STATS.lock().unwrap().clone(); // shallow
                    let mut lines = Vec::new();

                    for (id, s) in stats {
                        let prev = last_counts.insert(id.clone(), s.frames);
                        let fps  = match prev { Some(p) => (s.frames - p) as f32, None => s.frames as f32 };
                        lines.push((id, fps, s.points));
                    }

                    // ── render ──────────────────────────────────────
                    if !lines.is_empty() {
                        print!("\x1b[2J\x1b[H");          // clear screen
                        println!("──────── live streams ────────");
                        println!("{:<16} │ {:>6} │ POINTS", "STREAM", "FPS");
                        println!("──────────────────────────────");
                        for (id, fps, pts) in lines {
                            println!("{id:<16} │ {fps:>6.1} │ {pts}");
                        }
                        println!("──────────────────────────────");
                    }

                    last_refresh = Instant::now();
                }
            }
            println!("🛑  dashboard stopped");
        })?;

    /* ─── wait for user to press ENTER─── */
    println!("Press <ENTER> to quit …");
    while !SHUTTING_DOWN.load(Ordering::SeqCst) {
        // non-blocking check for Enter
        if let Ok(n) = std::io::stdin().read(&mut [0u8; 1]) {
            if n > 0 {
                SHUTTING_DOWN.store(true, Ordering::SeqCst);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    /* ─── join threads ─── */
    let _ = poller.join();
    let _ = consumer.join();
    let _ = dashboard.join();
    println!("🟢  Debug threads joined");

    /* ─── tidy up ─── */
    print!("Shutting down receiver … ");
    println!("unsubscribing from incoming frames …");
    ingress_unsubscribe();
    println!("unregistering debug callback …");
    unregister_debug_callback();
    println!("destroying receiver …");
    destroy();
    println!("✅  Stopped everything, waiting for certain threads to finish...");

    // Sleep for 3 seconds to allow any remaining threads to finish
    thread::sleep(Duration::from_secs(3));

    if let Err(e) = dump_threads() {
        eprintln!("Failed to list threads: {e}");
    }

    Ok(())
}

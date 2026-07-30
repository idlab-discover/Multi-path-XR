use circular_buffer::CircularBuffer;
use flute::core::UDPEndpoint;
use flute::receiver::{writer, MultiReceiver};
use metrics::get_metrics;
use pcf::types::PCF_MAGIC;
use prometheus::IntGauge;
use shared_networking::udp::{build_multicast_receiver, UdpRxOpts};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

use crate::clock::{ClockDomain, ClockOffsetSample, ClockSampleTrust, ClockSourceKey};
use crate::processing::ProcessingPipeline;
use crate::services::stream_manager::StreamManager;
use crate::storage::ReceiverTransport;

const FLUTE_OBJECT_STALE_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct FluteRobustnessMetrics {
    completed_objects_total: IntGauge,
    incomplete_objects_total: IntGauge,
    unrecoverable_objects_total: IntGauge,
    object_bytes_total: IntGauge,
}

impl FluteRobustnessMetrics {
    fn new() -> Self {
        let metrics = get_metrics();

        Self {
            completed_objects_total: metrics
                .get_or_create_gauge(
                    "flute_completed_objects_total",
                    "Total number of completed FLUTE objects delivered to the receiver",
                )
                .expect("flute_completed_objects_total"),
            incomplete_objects_total: metrics
                .get_or_create_gauge(
                    "flute_incomplete_objects_total",
                    "Total number of FLUTE objects dropped before completion due to error or timeout",
                )
                .expect("flute_incomplete_objects_total"),
            unrecoverable_objects_total: metrics
                .get_or_create_gauge(
                    "fec_unrecoverable_objects_total",
                    "Total number of FLUTE objects that remained unrecoverable and were dropped due to error or timeout",
                )
                .expect("fec_unrecoverable_objects_total"),
            object_bytes_total: metrics
                .get_or_create_gauge(
                    "flute_object_bytes_total",
                    "Total number of payload bytes delivered from completed FLUTE objects",
                )
                .expect("flute_object_bytes_total"),
        }
    }

    fn record_completed_object(&self, payload_bytes: usize) {
        self.completed_objects_total.inc();
        self.object_bytes_total
            .add((payload_bytes as u64).min(i64::MAX as u64) as i64);
    }

    fn record_unrecoverable_object(&self) {
        self.incomplete_objects_total.inc();
        self.unrecoverable_objects_total.inc();
    }

    fn reset(&self) {
        self.completed_objects_total.set(0);
        self.incomplete_objects_total.set(0);
        self.unrecoverable_objects_total.set(0);
        self.object_bytes_total.set(0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FluteObjectDisposition {
    Completed,
    Error,
    TimedOut,
    Pending,
}

fn classify_flute_object_disposition(
    is_complete: bool,
    has_error: bool,
    start_time: SystemTime,
    now: SystemTime,
) -> FluteObjectDisposition {
    if has_error {
        return FluteObjectDisposition::Error;
    }

    if is_complete {
        return FluteObjectDisposition::Completed;
    }

    match now.duration_since(start_time) {
        Ok(object_age) if object_age > FLUTE_OBJECT_STALE_AFTER => FluteObjectDisposition::TimedOut,
        _ => FluteObjectDisposition::Pending,
    }
}

fn current_time_us() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedClockObject {
    server_instance_id: Option<String>,
    remote_send_us: u64,
}

fn parse_clock_object(data: &[u8]) -> Option<ParsedClockObject> {
    let text = std::str::from_utf8(data).ok()?.trim();
    if let Some(remote_send_us) = text
        .strip_prefix("clock_us:")
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(ParsedClockObject {
            server_instance_id: None,
            remote_send_us,
        });
    }

    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let remote_send_us = value
        .get("remote_send_us")
        .or_else(|| value.get("remoteSendUs"))
        .and_then(serde_json::Value::as_u64)?;
    let server_instance_id = value
        .get("server_instance_id")
        .or_else(|| value.get("serverInstanceId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    Some(ParsedClockObject {
        server_instance_id,
        remote_send_us,
    })
}

pub struct FluteIngress {
    running: Arc<AtomicBool>,
    _circular_buffer: Arc<Mutex<CircularBuffer<32768, Vec<u8>>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    worker_handle: Mutex<Option<JoinHandle<()>>>,
    processing_pipeline: Arc<ProcessingPipeline>,
    robustness_metrics: FluteRobustnessMetrics,
    stream_id: String,
}
crate::log_drop!(FluteIngress);

impl FluteIngress {
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
    ) {
        let url: Option<String> = stream_manager.flute_url.read().unwrap().clone();
        if url.is_none() {
            error!("FLUTE URL is empty");
            return;
        }

        let url = url.unwrap();
        if !url.starts_with("udp://") {
            error!("Invalid FLUTE URL: '{}', must start with udp://", url);
            return;
        }

        let (ip, port) = match url.split_at(6) {
            ("udp://", rest) => {
                let mut parts = rest.split(':');
                let ip = parts.next().unwrap().to_string();
                let port = parts.next().unwrap_or("");
                let port: u16 = port.parse().expect("Invalid port number");
                (ip, port)
            }
            (_, "") => {
                error!("Invalid FLUTE URL: '{}', missing IP address and port", url);
                return;
            }
            _ => {
                error!("Invalid FLUTE URL: '{}', must start with udp://", url);
                return;
            }
        };

        let metrics = get_metrics();
        let reception_time_flute = metrics
            .get_or_create_gauge(
                "reception_time_flute",
                "Time it took to receive a FLUTE object.",
            )
            .unwrap();
        let robustness_metrics = FluteRobustnessMetrics::new();
        let stream_id = format!("flute_{ip}:{port}");
        processing_pipeline
            .activate_stream_for_transport(ReceiverTransport::Flute, stream_id.clone());

        let endpoint = UDPEndpoint::new(None, ip.clone(), port);

        let multicast_group: std::net::SocketAddr =
            format!("{}:{}", endpoint.destination_group_address, endpoint.port)
                .parse()
                .expect("invalid multicast group/port");

        let udp_socket = Arc::new(
            build_multicast_receiver(UdpRxOpts {
                group: multicast_group,
                read_timeout: Duration::from_millis(200),
                recv_buf_bytes: 8 * 1024 * 1024,
                reuse_port: true,
                v6_ifindex: None, // or Some(ifindex_by_name("en0")?)
                disable_loop: false,
                //..Default::default()
            })
            .expect("multicast Rx socket"),
        );

        let running = Arc::new(AtomicBool::new(true));
        let circular_buffer = Arc::new(Mutex::new(CircularBuffer::new()));
        let buffer_clone1 = Arc::clone(&circular_buffer);
        let buffer_clone2 = Arc::clone(&circular_buffer);
        let udp_socket_clone = Arc::clone(&udp_socket);
        let running_clone1 = Arc::clone(&running);
        let running_clone2 = Arc::clone(&running);

        // Packet reader thread
        let reader_handle = thread::Builder::new()
            .name("flute_reader".to_string())
            .spawn(move || {
                let mut buf = [0; 2048]; // This should be enough for the max FLUTE/ALC packet size.
                while running_clone1.load(Ordering::SeqCst) {
                    match udp_socket_clone.recv_from(&mut buf) {
                        // TODO: verify that this sleeps/yields while waiting for data
                        Ok((n, _)) => {
                            let mut buffer = buffer_clone1.lock().unwrap();
                            if buffer.is_full() {
                                error!("Circular buffer is full, dropping packet");
                                thread::yield_now(); // Allow other threads to run, in case the processor is busy
                                continue;
                            } else if n == 0 {
                                thread::yield_now(); // Allow other threads to run
                                continue;
                            } else if n == buf.len() {
                                // Packet is complete
                                // This could possibly indicate that the packet is too large and the last bytes are dropped by recv_from
                                // To be totally safe and correct, we should check with recvmsg(MSG_TRUNC).
                                warn!("Received packet of maximum size {}, possibly truncated", n);
                            }
                            buffer.push_back(buf[..n].to_vec());
                        }
                        Err(ref e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            /* just try again but yield first*/
                            thread::yield_now();
                        }
                        Err(e) => {
                            error!("Error receiving UDP packet: {:?}", e);
                        }
                    }
                }
                info!("Packet reader thread terminated");
            })
            .expect("Failed to spawn packet reader thread");

        let pipeline_clone = Arc::clone(&processing_pipeline);
        let worker_robustness_metrics = robustness_metrics.clone();
        let stream_id_clone = stream_id.clone();
        let flute_clock_source_id = Arc::new(Mutex::new(None::<String>));
        let worker_clock_source_id = Arc::clone(&flute_clock_source_id);

        let worker_handle = thread::Builder::new()
            .name("flute_worker".to_string())
            .spawn(move || {
                // MultiReceiver processing thread
                // MD5 Check is controlled from the server. If no MD5 is given, it will not be checked.
                let writer = Rc::new(writer::ObjectWriterBufferBuilder::new(true));
                let mut receiver = MultiReceiver::new(writer.clone(), None, false);
                let mut idle_loops: usize = 0;
                while running_clone2.load(Ordering::SeqCst) {
                    let packet = {
                        let mut buffer = buffer_clone2.lock().unwrap();
                        buffer.pop_front()
                    };
                    if let Some(data) = packet {
                        idle_loops = 0;
                        let now = SystemTime::now();
                        if let Err(e) = receiver.push(&endpoint, &data, now) {
                            error!("Error pushing data to receiver: {:?}", e);
                        }
                    } else {
                        idle_loops = idle_loops.saturating_add(1);

                        match idle_loops {
                            0..=10 => std::hint::spin_loop(),
                            11..=100 => thread::yield_now(),
                            _ => {
                                let sleep_time = match idle_loops {
                                    101..=200 => 50,
                                    201..=300 => 100,
                                    301..=400 => 500,
                                    401..=500 => 1_000,
                                    501..=600 => 5_000,
                                    601..=700 => 10_000,
                                    701..=800 => 50_000,
                                    _ => 100_000,
                                };
                                thread::sleep(std::time::Duration::from_micros(sleep_time));
                            }
                        }
                        continue;
                    }

                    let now = SystemTime::now();
                    receiver.cleanup(now);

                    let mut objects = writer.objects.borrow_mut();
                    for obj in objects.iter() {
                        let obj = obj.borrow();
                        if classify_flute_object_disposition(
                            obj.complete,
                            obj.error,
                            obj.start_time,
                            now,
                        ) == FluteObjectDisposition::Completed
                        {
                            let data: Vec<u8> = obj.data.clone();
                            if let Some(clock_object) = parse_clock_object(&data) {
                                if let Some(server_instance_id) =
                                    clock_object.server_instance_id.as_ref()
                                {
                                    *worker_clock_source_id.lock().unwrap() =
                                        Some(server_instance_id.clone());
                                }
                                if let Some(local_receive_us) = current_time_us() {
                                    let clock_source = clock_object
                                        .server_instance_id
                                        .map(|server_instance_id| {
                                            ClockSourceKey::with_server_id(
                                                ClockDomain::Flute,
                                                server_instance_id,
                                            )
                                        })
                                        .unwrap_or_else(|| {
                                            ClockSourceKey::for_transport(ClockDomain::Flute)
                                        });
                                    let _ = pipeline_clone.observe_clock_offset_sample(
                                        clock_source,
                                        ClockSampleTrust::LowOneWay,
                                        ClockOffsetSample {
                                            remote_now_us: clock_object.remote_send_us,
                                            local_send_us: local_receive_us,
                                            local_receive_us,
                                            server_wait_us: None,
                                        },
                                    );
                                }
                                continue;
                            }
                            worker_robustness_metrics.record_completed_object(data.len());
                            let receive_duration_us = obj
                                .end_time
                                .and_then(|end| end.duration_since(obj.start_time).ok())
                                .map(|d| d.as_micros() as i64)
                                .unwrap_or(0);
                            reception_time_flute.set(receive_duration_us);

                            let clock_source = worker_clock_source_id
                                .lock()
                                .unwrap()
                                .clone()
                                .map(|server_instance_id| {
                                    ClockSourceKey::with_server_id(
                                        ClockDomain::Flute,
                                        server_instance_id,
                                    )
                                })
                                .unwrap_or_else(|| {
                                    ClockSourceKey::for_transport(ClockDomain::Flute)
                                });

                            if data.starts_with(PCF_MAGIC) {
                                pipeline_clone.ingest_data_for_source(
                                    clock_source,
                                    stream_id_clone.clone(),
                                    0,
                                    0,
                                    0,
                                    data,
                                );
                                continue;
                            }

                            let filename = obj.meta.content_location.clone();
                            // filename is file://frame_{presentation_time}_{send_time}.bin"
                            // Extract the presentation_time and send_time from the filename
                            // Remove the frame_ prefix and .bin suffix
                            let filename = filename
                                .as_str()
                                .replace("file://frame_", "")
                                .trim_end_matches('/')
                                .replace(".bin", "");
                            let parts: Vec<&str> = filename.split('_').collect();
                            if parts.len() < 2 {
                                error!("Invalid filename format: {}", filename);
                                continue;
                            }
                            let presentation_time: u64 = match parts[0].parse() {
                                Ok(time) => time,
                                Err(_) => {
                                    error!("Invalid presentation time in filename: {}", filename);
                                    continue;
                                }
                            };
                            let send_time: u64 = match parts[1].parse() {
                                Ok(time) => time,
                                Err(_) => {
                                    error!("Invalid creation time in filename: {}", filename);
                                    continue;
                                }
                            };

                            pipeline_clone.ingest_data_for_source(
                                clock_source,
                                stream_id_clone.clone(),
                                0,
                                send_time,
                                presentation_time,
                                data,
                            );
                        }
                    }
                    objects.retain(|obj| {
                        let obj = obj.borrow();
                        match classify_flute_object_disposition(
                            obj.complete,
                            obj.error,
                            obj.start_time,
                            now,
                        ) {
                            FluteObjectDisposition::Completed => false,
                            FluteObjectDisposition::Error | FluteObjectDisposition::TimedOut => {
                                worker_robustness_metrics.record_unrecoverable_object();
                                false
                            }
                            FluteObjectDisposition::Pending => true,
                        }
                    });
                }
                info!("Processing thread terminated");
            })
            .expect("Failed to spawn processing thread");

        let ingress = Arc::new(Self {
            running,
            _circular_buffer: circular_buffer,
            reader_handle: Mutex::new(Some(reader_handle)),
            worker_handle: Mutex::new(Some(worker_handle)),
            processing_pipeline,
            robustness_metrics,
            stream_id,
        });

        stream_manager.set_flute_ingress(ingress);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);

        // join reader
        if let Some(h) = self.reader_handle.lock().unwrap().take() {
            let _ = h.join();
        }

        // join worker
        if let Some(h) = self.worker_handle.lock().unwrap().take() {
            let _ = h.join();
        }

        self.robustness_metrics.reset();
        self.processing_pipeline
            .remove_stream(self.stream_id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn ensure_metrics_initialized() {
        if catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok() {
            return;
        }

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = metrics::MetricsBuilder::new()
                .add_label("mode", "client-test")
                .build();
        }));

        assert!(
            catch_unwind(AssertUnwindSafe(metrics::get_metrics)).is_ok(),
            "failed to initialize global metrics for receiver tests"
        );
    }

    #[test]
    fn classify_flute_object_disposition_distinguishes_completed_error_and_timeout() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10);

        assert_eq!(
            classify_flute_object_disposition(true, false, now - Duration::from_secs(1), now,),
            FluteObjectDisposition::Completed,
        );
        assert_eq!(
            classify_flute_object_disposition(true, true, now - Duration::from_secs(1), now,),
            FluteObjectDisposition::Error,
        );
        assert_eq!(
            classify_flute_object_disposition(false, false, now - Duration::from_secs(6), now,),
            FluteObjectDisposition::TimedOut,
        );
        assert_eq!(
            classify_flute_object_disposition(false, false, now - Duration::from_secs(1), now,),
            FluteObjectDisposition::Pending,
        );
    }

    #[test]
    fn parse_clock_object_accepts_json_and_legacy_payloads() {
        let parsed =
            parse_clock_object(br#"{"server_instance_id":"server-a","remote_send_us":123456}"#)
                .expect("json clock object should parse");
        assert_eq!(parsed.server_instance_id.as_deref(), Some("server-a"));
        assert_eq!(parsed.remote_send_us, 123_456);

        let legacy = parse_clock_object(b"clock_us:42").expect("legacy clock object should parse");
        assert_eq!(legacy.server_instance_id, None);
        assert_eq!(legacy.remote_send_us, 42);
    }

    #[test]
    fn flute_robustness_metrics_record_completed_and_unrecoverable_objects() {
        ensure_metrics_initialized();

        let metrics = FluteRobustnessMetrics::new();
        metrics.reset();
        metrics.record_completed_object(1_024);
        metrics.record_unrecoverable_object();

        let registry = get_metrics();
        let completed_objects_total = registry
            .get_or_create_gauge(
                "flute_completed_objects_total",
                "Total number of completed FLUTE objects delivered to the receiver",
            )
            .unwrap();
        let incomplete_objects_total = registry
            .get_or_create_gauge(
                "flute_incomplete_objects_total",
                "Total number of FLUTE objects dropped before completion due to error or timeout",
            )
            .unwrap();
        let unrecoverable_objects_total = registry
            .get_or_create_gauge(
                "fec_unrecoverable_objects_total",
                "Total number of FLUTE objects that remained unrecoverable and were dropped due to error or timeout",
            )
            .unwrap();
        let object_bytes_total = registry
            .get_or_create_gauge(
                "flute_object_bytes_total",
                "Total number of payload bytes delivered from completed FLUTE objects",
            )
            .unwrap();

        assert_eq!(completed_objects_total.get(), 1);
        assert_eq!(incomplete_objects_total.get(), 1);
        assert_eq!(unrecoverable_objects_total.get(), 1);
        assert_eq!(object_bytes_total.get(), 1_024);

        metrics.reset();
        assert_eq!(completed_objects_total.get(), 0);
        assert_eq!(incomplete_objects_total.get(), 0);
        assert_eq!(unrecoverable_objects_total.get(), 0);
        assert_eq!(object_bytes_total.get(), 0);
    }
}

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};
use flute::core::UDPEndpoint;
use flute::receiver::{writer, MultiReceiver};
use metrics::get_metrics;
use tracing::{error, info, warn};
use circular_buffer::CircularBuffer;
use shared_networking::udp::{build_multicast_receiver, UdpRxOpts};

use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;

pub struct FluteIngress {
    running: Arc<AtomicBool>,
    _circular_buffer: Arc<Mutex<CircularBuffer<32768, Vec<u8>>>>,
    reader_handle:  Mutex<Option<JoinHandle<()>>>,
    worker_handle:  Mutex<Option<JoinHandle<()>>>,
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
            .get_or_create_gauge("reception_time_flute", "Time it took to receive a FLUTE object.")
            .unwrap();

        let endpoint = UDPEndpoint::new(None, ip.clone(), port);

        let multicast_group: std::net::SocketAddr = format!("{}:{}", endpoint.destination_group_address, endpoint.port)
            .parse()
            .expect("invalid multicast group/port");

        let udp_socket = Arc::new(build_multicast_receiver(UdpRxOpts {
            group: multicast_group,
            read_timeout: Duration::from_millis(200),
            recv_buf_bytes: 8 * 1024 * 1024,
            reuse_port: true,
            v6_ifindex: None,    // or Some(ifindex_by_name("en0")?)
            disable_loop: false,
            //..Default::default()
        }).expect("multicast Rx socket"));

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
                    match udp_socket_clone.recv_from(&mut buf) { // TODO: verify that this sleeps/yields while waiting for data
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
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut => { 
                                    /* just try again but yield first*/
                                    thread::yield_now();
                                }
                        Err(e) => {
                            error!("Error receiving UDP packet: {:?}", e);
                        }
                    }
                }
                info!("Packet reader thread terminated");
            }).expect("Failed to spawn packet reader thread");

        let pipeline_clone = Arc::clone(&processing_pipeline);
        let ip_clone = ip.clone();

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
                        if obj.complete && !obj.error {
                            let data: Vec<u8> = obj.data.clone();
                            let filename = obj.meta.content_location.clone();
                            // filename is file://frame_{presentation_time}_{send_time}.bin"
                            // Extract the presentation_time and send_time from the filename
                            // Remove the frame_ prefix and .bin suffix
                            let filename = filename.as_str().replace("file://frame_", "")
                            .trim_end_matches('/').replace(".bin", "");
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

                            let receive_duration_us = obj.end_time.and_then(|end| {
                                end.duration_since(obj.start_time).ok()
                            }).map(|d| d.as_micros() as i64).unwrap_or(0);
                            reception_time_flute.set(receive_duration_us);

                            pipeline_clone.ingest_data(
                                format!("flute_{ip_clone}:{port}"),
                                0,
                                send_time,
                                presentation_time,
                                data,
                            );
                        }
                    }
                    objects.retain(|obj| {
                        let obj = obj.borrow();
                        // Drop objects that are complete or errored
                        if obj.complete || obj.error {
                            return false;
                        }
                        // Do not retain when the object is too old. To prevent memory leaks
                        let now = SystemTime::now();
                        let five_seconds_ago = now - Duration::from_secs(5);
                        if obj.start_time < five_seconds_ago {
                            return false;
                        }
                        true
                    });
                }
                info!("Processing thread terminated");
            }).expect("Failed to spawn processing thread");

        let ingress = Arc::new(Self {
            running,
            _circular_buffer: circular_buffer,
            reader_handle:  Mutex::new(Some(reader_handle)),
            worker_handle:  Mutex::new(Some(worker_handle)),
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
    }
}

use std::net::UdpSocket;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::SystemTime;
use flute::core::UDPEndpoint;
use flute::receiver::{writer, MultiReceiver};
use metrics::get_metrics;
use tracing::{error, info};
use circular_buffer::CircularBuffer;

use crate::services::stream_manager::StreamManager;
use crate::processing::ProcessingPipeline;

pub struct FluteIngress {
    running: Arc<AtomicBool>,
    _circular_buffer: Arc<Mutex<CircularBuffer<32768, Vec<u8>>>>,
}

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
        let udp_socket = Arc::new(UdpSocket::bind(format!("{}:{}", endpoint.destination_group_address, endpoint.port))
            .expect("Failed to bind UDP socket"));

        let running = Arc::new(AtomicBool::new(true));
        let circular_buffer = Arc::new(Mutex::new(CircularBuffer::new()));
        let buffer_clone1 = Arc::clone(&circular_buffer);
        let buffer_clone2 = Arc::clone(&circular_buffer);
        let udp_socket_clone = Arc::clone(&udp_socket);
        let running_clone1 = Arc::clone(&running);
        let running_clone2 = Arc::clone(&running);

        // Packet reader thread
        thread::spawn(move || {
            let mut buf = [0; 2048];
            while running_clone1.load(Ordering::SeqCst) {
                match udp_socket_clone.recv_from(&mut buf) {
                    Ok((n, _)) => {
                        let mut buffer = buffer_clone1.lock().unwrap();
                        if buffer.is_full() {
                            error!("Circular buffer is full, dropping packet");
                            continue;
                        }
                        buffer.push_back(buf[..n].to_vec());
                    }
                    Err(e) => {
                        error!("Error receiving UDP packet: {:?}", e);
                    }
                }
            }
            info!("Packet reader thread terminated");
        });

        let pipeline_clone = Arc::clone(&processing_pipeline);
        let ip_clone = ip.clone();

        thread::spawn(move || {
            // MultiReceiver processing thread
            let writer = Rc::new(writer::ObjectWriterBufferBuilder::new());
            let mut receiver = MultiReceiver::new(writer.clone(), None, false);
            while running_clone2.load(Ordering::SeqCst) {
                let packet = {
                    let mut buffer = buffer_clone2.lock().unwrap();
                    buffer.pop_front()
                };
                if let Some(data) = packet {
                    let now = SystemTime::now();
                    if let Err(e) = receiver.push(&endpoint, &data, now) {
                        error!("Error pushing data to receiver: {:?}", e);
                    }
                }

                let now = SystemTime::now();
                receiver.cleanup(now);

                let mut objects = writer.objects.borrow_mut();
                for obj in objects.iter() {
                    let obj = obj.borrow();
                    if obj.complete && !obj.error {
                        let data: Vec<u8> = obj.data.clone();
                        let filename = obj.meta.content_location.clone();
                        // filename is file:///frame_{}_{}.bin", frame.presentation_time, frame.send_time
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

                        let receive_duration = obj.end_time.unwrap().duration_since(obj.start_time).unwrap();
                        reception_time_flute.set(receive_duration.as_micros() as i64);

                        pipeline_clone.ingest_data(
                            format!("flute_{}:{}", ip_clone, port),
                            0,
                            send_time,
                            presentation_time,
                            data,
                        );
                    }
                }
                objects.retain(|obj| {
                    let obj = obj.borrow();
                    !obj.complete || obj.error
                });
            }
            info!("Processing thread terminated");
        });

        let ingress = Arc::new(Self {
            running,
            _circular_buffer: circular_buffer,
        });

        stream_manager.set_flute_ingress(ingress);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

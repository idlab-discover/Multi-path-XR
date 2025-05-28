// egress/flute.rs

use std::{
    net::UdpSocket, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}
};

use crate::{
    encoders::EncodingFormat,
    processing::aggregator::PointCloudAggregator,
    processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
};

use shared_utils::types::{FrameTaskData, PointCloudData};

use circular_buffer::CircularBuffer;
use flute::{
    core::{lct::{Cenc, LCTHeader}, Oti, UDPEndpoint},
    sender::{Config, ObjectDesc, Sender},
};
use tracing::{info, debug, error, instrument};

use super::egress_common::{push_preencoded_frame_data, EgressCommonMetrics, EgressProtocol};

/// FLUTE Egress module responsible for sending frames over FLUTE protocol.
#[derive(Clone, Debug)]
pub struct FluteEgress {
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    packet_queue: Arc<Mutex<CircularBuffer<20000, Vec<u8>>>>,
    aggregator: Arc<PointCloudAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<Mutex<u32>>,
    encoding_format: Arc<Mutex<EncodingFormat>>,
    max_number_of_points: Arc<Mutex<u64>>,
    endpoint: Arc<Mutex<UDPEndpoint>>,
    sender: Arc<Mutex<Option<Sender>>>,
    udp_socket: Arc<Mutex<Option<UdpSocket>>>,
    content_encoding: Arc<Mutex<Cenc>>,
    fec: Arc<Mutex<String>>,
    fec_parity_percentage: Arc<Mutex<f32>>,
    bandwidth: Arc<Mutex<u32>>,
    latest_toi: Arc<Mutex<u128>>,
    fdt_id: Arc<Mutex<u32>>,
    md5: Arc<Mutex<bool>>,
    egress_metrics: Arc<EgressCommonMetrics>,
}

impl FluteEgress {
    /// Initializes the FLUTE Egress module.
    #[instrument(skip_all)]
    pub fn initialize(
        stream_manager: Arc<StreamManager>,
        processing_pipeline: Arc<ProcessingPipeline>,
        endpoint_url: String,
        port: u16,
    ) {
        let aggregator = Arc::new(PointCloudAggregator::new(stream_manager.clone()));

        let endpoint = UDPEndpoint::new(None, endpoint_url, port);
        let sender = None;
        let udp_socket = None;

        let instance = Arc::new(Self {
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            packet_queue: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(Mutex::new(30)),
            encoding_format: Arc::new(Mutex::new(EncodingFormat::Draco)),
            max_number_of_points: Arc::new(Mutex::new(100_000)),
            endpoint: Arc::new(Mutex::new(endpoint)),
            sender: Arc::new(Mutex::new(sender)),
            udp_socket: Arc::new(Mutex::new(udp_socket)),
            content_encoding: Arc::new(Mutex::new(Cenc::Null)),
            fec: Arc::new(Mutex::new("nocode".to_string())),
            fec_parity_percentage: Arc::new(Mutex::new(0.06)),
            bandwidth: Arc::new(Mutex::new(200_000_000)), // Default 200 Mbps
            latest_toi: Arc::new(Mutex::new(1)), // Start from 1
            fdt_id: Arc::new(Mutex::new(1)), // Start from 1
            md5: Arc::new(Mutex::new(true)), // Start from 1
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
        });

        // Store the instance in the StreamManager
        stream_manager.set_flute_egress(instance.clone());
    }

    /// Emits frame data over FLUTE protocol.
    #[instrument(skip_all)]
    fn emit_frame_data(&self, frame: FrameTaskData) {
        debug!(
            "Emitting frame with presentation time: {}",
            frame.presentation_time
        );


        //let start = std::time::Instant::now();
        // Initialize the FLUTE sender and UDP socket if not already done
        let mut sender_guard = self.sender.lock().unwrap();
        {
            let mut udp_socket_guard = self.udp_socket.lock().unwrap();

            if sender_guard.is_none() || udp_socket_guard.is_none() {

                // Create the FLUTE sender
                // Create UDP Socket
                let endpoint = self.endpoint.lock().unwrap().clone();

                let udp_socket_result = UdpSocket::bind("0.0.0.0:0");
                let Ok(socket) = udp_socket_result else {
                    error!("Failed to bind UDP socket: {:?}", udp_socket_result.err());
                    return;
                };
                //socket.set_nonblocking(true).unwrap();
                socket.set_multicast_ttl_v4(2).unwrap(); // TODO: make this configurable
                // socket.set_multicast_loop_v4(true).unwrap();

                // socket.join_multicast_v4(&endpoint.destination_group_address, None)?;

                socket.connect(format!(
                    "{}:{}",
                    endpoint.destination_group_address, endpoint.port
                )).unwrap();

                *udp_socket_guard = Some(socket);

                // Create FLUTE Sender
                let tsi = 1; // Transport Session Identifier
                let oti = self.create_oti(self.fec.lock().unwrap().clone(), *self.fec_parity_percentage.lock().unwrap());
                let config = Config {
                    toi_initial_value: Some(*self.latest_toi.lock().unwrap()),
                    fdt_start_id: *self.fdt_id.lock().unwrap(),
                    // fdt_publish_mode: flute::sender::FDTPublishMode::Automatic,
                    ..Default::default()
                };

                let sender = Sender::new(endpoint.clone(), tsi, &oti, &config);

                *sender_guard = Some(sender);

                debug!("FLUTE sender and UDP socket initialized");
            }
        }

        let sender = sender_guard.as_mut().unwrap();
        //let udp_socket = udp_socket_guard.as_mut().unwrap();

        let content_encoding = *self.content_encoding.lock().unwrap();

        // Prepare the frame data as an ObjectDesc
        let now = SystemTime::now();
        let uri = format!("file://frame_{}_{}.bin", frame.presentation_time, frame.send_time);
        // Convert the frame to JSON and then to bytes
        //let bytes = serde_json::to_string(&frame).unwrap().as_bytes().to_vec();
        debug!("Frame data as JSON converted to a vector of {} bytes", frame.data.len());
        let obj = ObjectDesc::create_from_buffer(
            frame.data,
            "application/octet-stream",
            &url::Url::parse(&uri).unwrap(),
            1,
            None,
            None, // TODO: check if any of these fields need to be set
            None,
            None,
            content_encoding,
            true,
            None,
            *self.md5.lock().unwrap(),
        )
        .unwrap();

        debug!("Frame data prepared as ObjectDesc");

        // Add object(s) (frames) to the FLUTE sender (priority queue 0)
        let toi = sender.add_object(0, obj);
        if toi.is_err() {
            error!("Failed to add object to FLUTE sender");
            return;
        }

        let toi = toi.unwrap();

        //info!("Object added to FLUTE sender with TOI: {}", toi);

        // Update the latest TOI
        let mut latest_toi = self.latest_toi.lock().unwrap();
        // If the TOI is greater than the latest TOI, update it
        if toi > *latest_toi {
            *latest_toi = toi;
        }
        

        // t/*
        // Always call publish after adding objects, if fdt publish mode is manual
        let fdt_publish = sender.publish(now);
        if fdt_publish.is_err() {
            error!("Failed to publish FDT: {:?}", fdt_publish.err());
            return;
        }

        debug!("FDT published");
        //*/

        // Increment the FDT ID
        let mut fdt_id = self.fdt_id.lock().unwrap();
        *fdt_id = (*fdt_id + 1) & 0xFFFFF;



        //let elapsed = start.elapsed();
        //info!("Frame conversion took: {:?} ms", elapsed);

        let mut fdt_pkts: Vec<Vec<u8>> = vec![];
        let mut file_pkt_count = 0;
        while let Some(pkt) = sender.read(now) {
            if pkt.is_empty() {
                break;
            }
            let lct_header = crate::egress::flute::FluteEgress::parse_lct_header(&pkt);
            if let Ok(lct_header) = lct_header {
                if lct_header.toi == 0 {
                    // Clone the packet into the fdt_pkts vector
                    fdt_pkts.push(pkt.clone());
                } else {
                    file_pkt_count += 1;
                }
            }

            let mut attempts = 0;
            loop {
                {
                    // Use a small scope to release the lock each iteration
                    let mut queue = self.packet_queue.lock().unwrap();
                    if !queue.is_full() {
                        queue.push_back(pkt);
                        break;
                    }
                }
                attempts += 1;
                if attempts > 1000 {
                    break;
                }
                // debug!("Packet queue is full, waiting for space...");
                // Waiting outside the scope to prevent busy-waiting with an active lock
                thread::sleep(Duration::from_micros(100));
            }
            if attempts > 1000 {
                error!("Packet queue is full and has not been emptied for a long time, dropping frame packets");
                break;
            }
        }
        // Only retransmit FDT packets if they are worth sending.
        // Small files that only have a few packets, are probably not significant
        // and thus not worth the extra overhead.
        if !fdt_pkts.is_empty() && file_pkt_count > 3 {
            // Retransmit the FDT packets by pushing them to the packet queue
            for pkt in fdt_pkts {
                // Use a small scope to release the lock each iteration
                let mut queue = self.packet_queue.lock().unwrap();
                if queue.is_full() {
                    break;
                }
                queue.push_back(pkt.clone());
            }
        } else {
            error!("No FDT packets received");
        }

        //let elapsed = start.elapsed();
        //info!("Frame emission took: {:?} ms", elapsed);

        debug!("Frame emitted with send time: {}, presentation time: {} and toi {}", frame.send_time, frame.presentation_time, toi);

        // Remove the object from the FLUTE sender
        let _ = sender.remove_object(toi);

        debug!("Object removed from FLUTE sender");
    }

    /// This thread continuously takes from `packet_queue` and sends to `udp_socket`,
    /// respecting a bandwidth limit via a simpler mechanism.
    /// The loop that implements rate-limiting and sends packets from `packet_queue`.
    #[instrument(skip_all)]
    fn packet_transmitter_loop(&self) {
        // Keep track of when we last sent a packet, to measure actual time between sends.
        let mut last_send_instant = Instant::now();

        // Read the bandwidth from your Arc<Mutex<u32>> only once every few iterations.
        let mut bandwidth_bps = {
            *self.bandwidth.lock().unwrap()
        };
        let mut iteration_count = 0;

        info!("Starting packet_transmitter_loop");
        loop {
            // Optional: check for a shutdown
            /*if self.shutdown_flag.load(Ordering::Relaxed) {
                break;
            }*/

            // 1) Pop a packet from the queue (if any).
            let maybe_packet = {
                let mut q = self.packet_queue.lock().unwrap();
                q.pop_front()
            };

            // If the queue is empty, sleep briefly and try again
            let packet = match maybe_packet {
                Some(p) => p,
                None => {
                    thread::sleep(Duration::from_micros(2000));
                    continue;
                }
            };

            let packet_size_bytes = packet.len() as u64;

            // 2) Send the packet over UDP.
            {
                let mut socket_guard = self.udp_socket.lock().unwrap();
                if let Some(ref mut udp_socket) = *socket_guard {
                    if let Err(e) = udp_socket.send(&packet) {
                        error!("Failed to send FLUTE packet: {:?}", e);
                    }
                } else {
                    error!("No UDP socket available in packet_transmitter_loop");
                }
            }

            // 3) Bandwidth re-check every N iterations (e.g., 10)
            iteration_count += 1;
            if iteration_count >= 100 {
                iteration_count = 0;
                bandwidth_bps = *self.bandwidth.lock().unwrap();
            }

            // 4) Calculate how long we *want* to wait, based on packet size & bandwidth
            //    Suppose bandwidth_bps is bits/second.  We'll compute time in milliseconds.

            // a) Compute how long, in ms, it *should* take to send `packet_size` bytes at `bandwidth_bps`.
            // bits needed for this packet
            let bits_needed = packet_size_bytes.saturating_mul(8);
            // Microseconds needed for this packet at the given bandwidth
            let desired_us_for_packet = if bandwidth_bps == 0 {
                0 // if user set bandwidth=0, we can interpret as "send immediately" or handle differently
            } else {
                // Multiply by 1_000_000 to convert seconds to milliseconds
                bits_needed.saturating_mul(1_000_000) / bandwidth_bps as u64
            };

            // b) How much time has actually elapsed since our last send?
            let now: Instant = Instant::now();
            let elapsed_since_last_send = now.duration_since(last_send_instant).as_micros() as u64;

            // c) If we haven't “spent” enough time, sleep the difference
            if desired_us_for_packet > elapsed_since_last_send {
                let sleep_us = desired_us_for_packet - elapsed_since_last_send;
                // debug!("Sleeping for {} us to respect bandwidth limit", sleep_ms);
                if sleep_us > 100 {
                    thread::sleep(Duration::from_micros(sleep_us));
                }
            }

            // d) Now update the "last send" instant to *right now* (after sleeping).
            last_send_instant = Instant::now();
        }
        // End of loop
        // info!("packet_transmitter_loop is exiting (shutdown or error).");
    }

    /// Sets the content encoding for the egress.
    #[instrument(skip_all)]
    pub fn set_content_encoding(&self, content_encoding: String) {
        *self.content_encoding.lock().unwrap() = match content_encoding.to_lowercase().as_str() {
            "null" => Cenc::Null,
            "zlib" => Cenc::Zlib,
            "deflate" => Cenc::Deflate,
            "gzip" => Cenc::Gzip,
            _ => Cenc::Null,
            
        };
    }

    #[instrument(skip_all)]
    pub fn set_fec(&self, fec: String) {
        *self.fec.lock().unwrap() = fec;
    }

    #[instrument(skip_all)]
    pub fn set_fec_parity_percentage(&self, fec_parity_percentage: f32) {
        *self.fec_parity_percentage.lock().unwrap() = fec_parity_percentage;
    }

    #[instrument(skip_all)]
    pub fn set_bandwidth(&self, bandwidth: u32) {
        *self.bandwidth.lock().unwrap() = bandwidth;
    }

    #[instrument(skip_all)]
    pub fn destroy_sender(&self) {
        let mut sender_guard = self.sender.lock().unwrap();
        let mut udp_socket_guard = self.udp_socket.lock().unwrap();

        // Just forget about both by setting them to None
        *sender_guard = None;
        *udp_socket_guard = None;
    }

    #[instrument(skip_all)]
    fn create_oti(&self, fec: String, parity_percentage: f32) -> Oti {
        let fec_encoding_symbol_length = 1400;
        let fec_max_source_block_length = 60;
        // We will round up to the nearest integer
        let fec_max_parity_symbols = (fec_max_source_block_length as f32 * parity_percentage).ceil() as u16;

        match fec.to_lowercase().as_str() {
            "raptor" => Oti::new_raptor(
                fec_encoding_symbol_length, 
                fec_max_source_block_length, 
                fec_max_parity_symbols, 
                1, 
                4).unwrap(),
            "raptorq" => Oti::new_raptorq(
                fec_encoding_symbol_length, 
                fec_max_source_block_length, 
                fec_max_parity_symbols, 
                1, 
                4).unwrap(),
            "reedsolomongf28" => Oti::new_reed_solomon_rs28(
                fec_encoding_symbol_length, 
                fec_max_source_block_length.try_into().unwrap(), 
                fec_max_parity_symbols.try_into().unwrap()).unwrap(),
            "reedsolomongf28underspecified" => Oti::new_reed_solomon_rs28_under_specified(
                fec_encoding_symbol_length, 
                fec_max_source_block_length, 
                fec_max_parity_symbols).unwrap(),
            "nocode" => Oti::new_no_code(1424, 64),
            _ => Oti::new_no_code(1424, 64),
        }
    }

/*    /// Sets the OTI (FEC parameters).
    pub async fn set_oti(&self, oti: Oti) {
        // Update the OTI in the sender
        // Need to reinitialize the sender
        let mut sender_guard = self.sender.lock().unwrap();
        if let Some(sender) = sender_guard.as_mut() {
            sender.update_oti(&oti);
        }
    }
*/

    /// Sets the MD5 flag.
    #[instrument(skip_all)]
    pub fn set_md5(&self, md5: bool) {
        *self.md5.lock().unwrap() = md5;
    }

    fn parse_lct_header(data: &[u8]) -> Result<LCTHeader, String> {
        /*
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         *  |   V   | C |PSI|S| O |H|Res|A|B|   HDR_LEN     | Codepoint (CP)|
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         *  | Congestion Control Information (CCI, length = 32*(C+1) bits)  |
         *  |                          ...                                  |
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         *  |  Transport Session Identifier (TSI, length = 32*S+16*H bits)  |
         *  |                          ...                                  |
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         *  |   Transport Object Identifier (TOI, length = 32*O+16*H bits)  |
         *  |                          ...                                  |
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         *  |                Header Extensions (if applicable)              |
         *  |                          ...                                  |
         *  +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
         */
    
        let len = data.get(2).map_or_else(
            || Err("Fail to read lct header size"),
            |&v| Ok((v as usize) << 2),
        )?;
    
        if len > data.len() {
            return Err(format!(
                "lct header size is {} whereas pkt size is {}",
                len,
                data.len()
            ));
        }
    
        let cp = data[3];
        let flags1 = data[0];
        let flags2 = data[1];
    
        let s = (flags2 >> 7) & 0x1;
        let o = (flags2 >> 5) & 0x3;
        let h = (flags2 >> 4) & 0x1;
        let c = (flags1 >> 2) & 0x3;
        let a = (flags2 >> 1) & 0x1;
        let b = flags2 & 0x1;
        let version = flags1 >> 4;
        if version != 1 && version != 2 {
            return Err(format!(
                "FLUTE version {} is not supported",
                version
            ));
        }
    
        let cci_len = ((c + 1) as u32) << 2;
        let tsi_len = ((s as u32) << 2) + ((h as u32) << 1);
        let toi_len = ((o as u32) << 2) + ((h as u32) << 1);
    
        let cci_from: usize = 4;
        let cci_to: usize = (4 + cci_len) as usize;
        let tsi_to: usize = cci_to + tsi_len as usize;
        let toi_to: usize = tsi_to + toi_len as usize;
        let header_ext_offset = toi_to as u32;
    
        if toi_to > data.len() || cci_len > 16 || tsi_len > 8 || toi_len > 16 {
            return Err(format!(
                "toi ends to offset {} whereas pkt size is {}",
                toi_to,
                data.len()
            ));
        }
    
        if header_ext_offset > len as u32 {
            return Err("EXT offset outside LCT header".to_owned());
        }
    
        let mut cci: [u8; 16] = [0; 16]; // Store up to 128 bits
        let mut tsi: [u8; 8] = [0; 8]; // Store up to 64 bits
        let mut toi: [u8; 16] = [0; 16]; // Store up to 128 bits
    
        let _ = &cci[(16 - cci_len) as usize..].copy_from_slice(&data[cci_from..cci_to]);
        let _ = &tsi[(8 - tsi_len) as usize..].copy_from_slice(&data[cci_to..tsi_to]);
        let _ = &toi[(16 - toi_len) as usize..].copy_from_slice(&data[tsi_to..toi_to]);
    
        let cci = u128::from_be_bytes(cci);
        let tsi = u64::from_be_bytes(tsi);
        let toi = u128::from_be_bytes(toi);
    
        Ok(LCTHeader {
            len,
            cci,
            tsi,
            toi,
            cp,
            close_object: b != 0,
            close_session: a != 0,
            header_ext_offset,
            length: len,
        })
    }
}


impl EgressProtocol for FluteEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        *self.encoding_format.lock().unwrap()
    }

    #[inline]
    fn max_number_of_points(&self) -> u64 {
        *self.max_number_of_points.lock().unwrap()
    }

    fn ensure_threads_started(&self) {
        let already_started = self.threads_started.load(Ordering::Relaxed);
        if already_started {
            return;
        }

        // Set the threads as started
        self.threads_started.store(true, Ordering::Relaxed);

        // Start background threads using the common module
        crate::egress::egress_common::start_generator_thread(
            "FLT_E".to_string(),
            self.processing_pipeline.clone(),
            self.aggregator.clone(),
            self.frame_buffer.clone(),
            self.fps.clone(),
            self.encoding_format.clone(),
            self.max_number_of_points.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "FLT_E".to_string(),
            self.frame_buffer.clone(),
            move |frame| {
                self_clone.emit_frame_data(frame);
            },
            false
        );

        let self_clone = self.clone();
        thread::spawn(move || {
            self_clone.packet_transmitter_loop();
        });
    }

    fn push_point_cloud(&self, point_cloud: PointCloudData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator.update_point_cloud(stream_id, point_cloud);
    }

    // Process and sends a frame, this raw version bypasses the aggregation
    fn push_encoded_frame(&self, raw_data: Vec<u8>, _stream_id: String, mut creation_time: u64, presentation_time: u64, ring_buffer_bypass: bool, client_id: Option<u64>, tile_index: Option<u32>) {
        // Ensure the threads are started
        self.ensure_threads_started();

        let self_clone = self.clone();
        let bypass = if ring_buffer_bypass {

            let since_the_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards");
            creation_time = since_the_epoch.as_micros() as u64;

            Some(Box::new(move |frame| {
                self_clone.emit_frame_data(frame);
            }) as Box<dyn Fn(FrameTaskData) + Send + 'static>)
        } else {
            None
        };
        
        // Then call the “push_preencoded_frame_data”:
        push_preencoded_frame_data(
            "FLT_E",
            &self.frame_buffer,
            creation_time,
            presentation_time,
            raw_data, // data is moved
            bypass,
            self.egress_metrics.bytes_to_send.clone(),
            self.egress_metrics.frame_drops_full_egress_buffer.clone(),
            self.egress_metrics.number_of_combined_frames.clone(),
            client_id,
            tile_index,
        );
    }

    fn set_fps(&self, fps: u32) {
        *self.fps.lock().unwrap() = fps;
    }

    fn set_encoding_format(&self, encoding_format: EncodingFormat) {
        *self.encoding_format.lock().unwrap() = encoding_format;
    }

    fn set_max_number_of_points(&self, max_number_of_points: u64) {
        *self.max_number_of_points.lock().unwrap() = max_number_of_points;
    }
}
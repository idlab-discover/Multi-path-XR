// egress/flute.rs

use std::{
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    processing::aggregator::SpatialFrameAggregator, processing::ProcessingPipeline,
    services::stream_manager::StreamManager,
};

use shared_utils::types::{FramePayloadMetadata, FrameTaskData, SpatialFrameData};
use spatial_codecs::encoder::EncodingFormat;

use circular_buffer::CircularBuffer;
use flute::{
    core::{
        lct::{Cenc, LCTHeader},
        Oti, UDPEndpoint,
    },
    sender::{Config, ObjectDesc, Sender},
};
use shared_networking::udp::{build_multicast_sender, UdpTxOpts};
use tracing::{debug, error, info, instrument};

use super::egress_common::{
    frame_task_to_pcf_wire, push_preencoded_frame_data, AtomicEncodingFormat, EgressCommonMetrics,
    EgressProtocol,
};

const FLUTE_CLOCK_OBJECT_INTERVAL: Duration = Duration::from_millis(250);

fn current_time_us() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
}

#[derive(Debug)]
struct AtomicCenc {
    inner: AtomicU8,
}

impl AtomicCenc {
    fn new(cenc: Cenc) -> Self {
        Self {
            inner: AtomicU8::new(Self::encode(cenc)),
        }
    }

    #[inline]
    fn load(&self) -> Cenc {
        Self::decode(self.inner.load(Ordering::Relaxed))
    }

    #[inline]
    fn store(&self, cenc: Cenc) {
        self.inner.store(Self::encode(cenc), Ordering::Relaxed);
    }

    #[inline]
    const fn encode(cenc: Cenc) -> u8 {
        cenc as u8
    }

    #[inline]
    const fn decode(raw: u8) -> Cenc {
        match raw {
            0 => Cenc::Null,
            1 => Cenc::Zlib,
            2 => Cenc::Deflate,
            3 => Cenc::Gzip,
            _ => Cenc::Null,
        }
    }
}

/// FLUTE Egress module responsible for sending frames over FLUTE protocol.
#[derive(Clone, Debug)]
pub struct FluteEgress {
    processing_pipeline: Arc<ProcessingPipeline>,
    frame_buffer: Arc<Mutex<CircularBuffer<10, FrameTaskData>>>,
    packet_queue: Arc<Mutex<CircularBuffer<20000, Vec<u8>>>>,
    aggregator: Arc<SpatialFrameAggregator>,
    threads_started: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
    encoding_format: Arc<AtomicEncodingFormat>,
    max_number_of_primitives: Arc<AtomicU64>,
    // TODO: check if the mutexes below can also be RwLocks
    endpoint: Arc<Mutex<UDPEndpoint>>,
    sender: Arc<Mutex<Option<Sender>>>,
    udp_socket: Arc<Mutex<Option<UdpSocket>>>,
    content_encoding: Arc<AtomicCenc>,
    fec: Arc<Mutex<String>>,
    fec_parity_percentage: Arc<Mutex<f32>>,
    bandwidth: Arc<AtomicU32>,
    latest_toi: Arc<Mutex<u128>>,
    fdt_id: Arc<Mutex<u32>>,
    md5: Arc<AtomicBool>,
    last_clock_object_at: Arc<Mutex<Option<Instant>>>,
    server_instance_id: Arc<String>,
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
        server_instance_id: Arc<String>,
    ) {
        let aggregator = Arc::new(SpatialFrameAggregator::new(stream_manager.clone()));

        let endpoint = UDPEndpoint::new(None, endpoint_url, port);
        let sender = None;
        let udp_socket = None;

        let instance = Arc::new(Self {
            processing_pipeline: processing_pipeline.clone(),
            frame_buffer: Arc::new(Mutex::new(CircularBuffer::new())),
            packet_queue: Arc::new(Mutex::new(CircularBuffer::new())),
            aggregator: aggregator.clone(),
            threads_started: Arc::new(AtomicBool::new(false)),
            fps: Arc::new(AtomicU32::new(30)),
            encoding_format: Arc::new(AtomicEncodingFormat::new(EncodingFormat::Draco)),
            max_number_of_primitives: Arc::new(AtomicU64::new(100_000)),
            endpoint: Arc::new(Mutex::new(endpoint)),
            sender: Arc::new(Mutex::new(sender)),
            udp_socket: Arc::new(Mutex::new(udp_socket)),
            content_encoding: Arc::new(AtomicCenc::new(Cenc::Null)),
            fec: Arc::new(Mutex::new("nocode".to_string())),
            fec_parity_percentage: Arc::new(Mutex::new(0.06)),
            bandwidth: Arc::new(AtomicU32::new(200_000_000)), // Default 200 Mbps
            latest_toi: Arc::new(Mutex::new(1)),              // Start from 1
            fdt_id: Arc::new(Mutex::new(1)),                  // Start from 1
            md5: Arc::new(AtomicBool::new(false)),            // By default, MD5 is disabled
            last_clock_object_at: Arc::new(Mutex::new(None)),
            server_instance_id,
            egress_metrics: Arc::new(EgressCommonMetrics::new()),
        });

        // Store the instance in the StreamManager
        stream_manager.set_flute_egress(instance.clone());
    }

    /// This thread continuously takes from `packet_queue` and sends to `udp_socket`,
    /// respecting a bandwidth limit via a simpler mechanism.
    /// The loop that implements rate-limiting and sends packets from `packet_queue`.
    #[instrument(skip_all)]
    fn packet_transmitter_loop(&self) {
        // Keep track of when we last sent a packet, to measure actual time between sends.
        let mut last_send_instant = Instant::now();

        // Read the bandwidth from your Arc<Mutex<u32>> only once every few iterations.
        let mut bandwidth_bps = self.bandwidth.load(Ordering::Relaxed);
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
                bandwidth_bps = self.bandwidth.load(Ordering::Relaxed);
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
        let content_encoding = match content_encoding.to_lowercase().as_str() {
            "null" => Cenc::Null,
            "zlib" => Cenc::Zlib,
            "deflate" => Cenc::Deflate,
            "gzip" => Cenc::Gzip,
            _ => Cenc::Null,
        };
        self.content_encoding.store(content_encoding);
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
        self.bandwidth.store(bandwidth, Ordering::Relaxed);
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
        let fec_max_parity_symbols =
            (fec_max_source_block_length as f32 * parity_percentage).ceil() as u16;

        match fec.to_lowercase().as_str() {
            "raptor" => Oti::new_raptor(
                fec_encoding_symbol_length,
                fec_max_source_block_length,
                fec_max_parity_symbols,
                1,
                4,
            )
            .unwrap(),
            "raptorq" => Oti::new_raptorq(
                fec_encoding_symbol_length,
                fec_max_source_block_length,
                fec_max_parity_symbols,
                1,
                4,
            )
            .unwrap(),
            "reedsolomongf28" => Oti::new_reed_solomon_rs28(
                fec_encoding_symbol_length,
                fec_max_source_block_length.try_into().unwrap(),
                fec_max_parity_symbols.try_into().unwrap(),
            )
            .unwrap(),
            "reedsolomongf28underspecified" => Oti::new_reed_solomon_rs28_under_specified(
                fec_encoding_symbol_length,
                fec_max_source_block_length,
                fec_max_parity_symbols,
            )
            .unwrap(),
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
        self.md5.store(md5, Ordering::Relaxed);
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
            return Err(format!("FLUTE version {version} is not supported"));
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

    fn maybe_add_clock_object(&self, sender: &mut Sender) {
        let now_instant = Instant::now();
        {
            let mut last_clock_object_at = self.last_clock_object_at.lock().unwrap();
            if last_clock_object_at.is_some_and(|last| {
                now_instant.saturating_duration_since(last) < FLUTE_CLOCK_OBJECT_INTERVAL
            }) {
                return;
            }
            *last_clock_object_at = Some(now_instant);
        }

        let Some(now_us) = current_time_us() else {
            return;
        };
        let uri = format!("file://clock_{now_us}.txt");
        let Ok(url) = url::Url::parse(&uri) else {
            return;
        };
        let obj = ObjectDesc::create_from_buffer(
            serde_json::json!({
                "server_instance_id": self.server_instance_id.as_str(),
                "remote_send_us": now_us,
            })
            .to_string()
            .into_bytes(),
            "text/plain",
            &url,
            1,
            None,
            None,
            None,
            None,
            Cenc::Null,
            true,
            None,
            false,
        )
        .unwrap();

        if let Err(err) = sender.add_object(0, obj) {
            error!("Failed to add FLUTE clock object: {:?}", err);
        } else {
            debug!("FLUTE clock object queued");
        }
    }
}

impl EgressProtocol for FluteEgress {
    #[inline]
    fn encoding_format(&self) -> EncodingFormat {
        self.encoding_format.load()
    }

    #[inline]
    fn max_number_of_primitives(&self) -> u64 {
        self.max_number_of_primitives.load(Ordering::Relaxed)
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
            self.max_number_of_primitives.clone(),
        );

        let self_clone = self.clone();
        crate::egress::egress_common::start_transmission_thread(
            "FLT_E".to_string(),
            self.frame_buffer.clone(),
            move |frame| {
                self_clone.emit_frame_data(frame);
            },
            false,
        );

        let self_clone = self.clone();
        thread::Builder::new()
            .name("flute_transmitter".to_string())
            .spawn(move || {
                self_clone.packet_transmitter_loop();
            })
            .expect("Failed to spawn flute_transmitter thread");
    }

    fn push_spatial_frame(&self, spatial_frame: SpatialFrameData, stream_id: String) {
        self.ensure_threads_started();
        self.aggregator
            .update_spatial_frame(stream_id, spatial_frame);
    }

    // Process and sends a frame, this raw version bypasses the aggregation
    fn push_encoded_frame(
        &self,
        raw_data: Vec<u8>,
        _stream_id: String,
        mut creation_time: u64,
        presentation_time: u64,
        ring_buffer_bypass: bool,
        payload_metadata: Option<FramePayloadMetadata>,
        client_id: Option<u64>,
        quality_index: Option<u32>,
    ) {
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
            })
                as Box<dyn Fn(FrameTaskData) + Send + 'static>)
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
            payload_metadata,
            bypass,
            self.egress_metrics.as_ref(),
            client_id,
            quality_index,
        );
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
                let ttl = 2; // TODO: make configurable
                             // Create the FLUTE sender
                             // Create UDP Socket
                let endpoint = self.endpoint.lock().unwrap().clone();

                let dst: std::net::SocketAddr =
                    format!("{}:{}", endpoint.destination_group_address, endpoint.port)
                        .parse()
                        .expect("invalid FLUTE dst");
                let socket = build_multicast_sender(UdpTxOpts {
                    dst,
                    ttl_v4: Some(ttl),
                    hops_v6: Some(ttl),
                    v4_if: None,
                    v6_ifindex: None,
                    snd_buf_bytes: 8 * 1024 * 1024,
                    disable_loop: false,
                    //..Default::default()
                })
                .expect("multicast Tx socket");

                *udp_socket_guard = Some(socket);

                // Create FLUTE Sender
                let tsi = 1; // Transport Session Identifier
                let oti = self.create_oti(
                    self.fec.lock().unwrap().clone(),
                    *self.fec_parity_percentage.lock().unwrap(),
                );
                let config = Config {
                    toi_initial_value: Some(*self.latest_toi.lock().unwrap()),
                    fdt_start_id: *self.fdt_id.lock().unwrap(),
                    // We could change the publish mode to ObjectsBeingTransferred instead of FullFDT.
                    // However, we already remove objects from the FDT after sending them, so it should not matter.
                    // TODO: verify this assumption. We might reduce some overhead by changing the mode.
                    // If we use ObjectsBeingTransferred, then we need to remove the call to publish
                    // as the library then automatically calls publish when adding objects.
                    // fdt_publish_mode: flute::sender::FDTPublishMode::ObjectsBeingTransferred,
                    ..Default::default()
                };

                let sender = Sender::new(endpoint.clone(), tsi, &oti, &config);

                *sender_guard = Some(sender);

                debug!("FLUTE sender and UDP socket initialized");
            }
        }

        let sender = sender_guard.as_mut().unwrap();
        //let udp_socket = udp_socket_guard.as_mut().unwrap();

        let content_encoding = self.content_encoding.load();

        // Prepare the frame data as an ObjectDesc
        let now = SystemTime::now();
        let uri = format!("file://f_{}.bin", frame.presentation_time);
        let payload = match frame_task_to_pcf_wire(&frame) {
            Ok(payload) => payload,
            Err(err) => {
                error!("Failed to encode FLUTE frame as PCF: {}", err);
                return;
            }
        };
        debug!(
            "Frame data converted to a PCF payload of {} bytes",
            payload.len()
        );
        let obj = ObjectDesc::create_from_buffer(
            payload,
            "application/octet-stream",
            &url::Url::parse(&uri).unwrap(),
            1,
            None,
            // TODO: check if any of these fields need to be set
            None, // e.g. the target acquisition could be used to spread out the data over a window, such as one frame time.
            None,
            None,
            content_encoding,
            true,
            None,
            self.md5.load(Ordering::Relaxed),
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
        drop(latest_toi);

        self.maybe_add_clock_object(sender);

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

        debug!(
            "Frame emitted with send time: {}, presentation time: {} and toi {}",
            frame.send_time, frame.presentation_time, toi
        );

        // Remove the object from the FLUTE sender
        let _ = sender.remove_object(toi);

        debug!("Object removed from FLUTE sender");
    }

    fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::Relaxed);
    }

    fn set_encoding_format(&self, encoding_format: EncodingFormat) {
        self.encoding_format.store(encoding_format);
    }

    fn set_max_number_of_primitives(&self, max_number_of_primitives: u64) {
        self.max_number_of_primitives
            .store(max_number_of_primitives, Ordering::Relaxed);
    }
}

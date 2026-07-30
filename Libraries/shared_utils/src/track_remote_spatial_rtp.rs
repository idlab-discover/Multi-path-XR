use bitvec::prelude::*;
use webrtc::error::Error as RtcError;
use webrtc::track::track_remote::TrackRemote;

use crate::types::{FramePayloadMetadata, FrameTaskData};
use dashmap::DashMap;
use std::{sync::Arc, time::Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// The same layout as the spatial RTP payloader header:
///   [0..8]   frame_nr
///   [8..12]  total_len
///   [12..16] seq_offset
///   [16..20] chunk_len
///
/// Then chunk_len bytes of data.
#[derive(Clone, Debug, Default)]
pub struct DepacketHeader {
    pub frame_nr: u64,
    pub total_len: u32,
    pub offset: u32,
    pub chunk_len: u32,
}

impl DepacketHeader {
    pub const HEADER_SIZE: usize = 20;

    pub fn parse(packet_payload: &[u8]) -> Option<(Self, &[u8])> {
        if packet_payload.len() < Self::HEADER_SIZE {
            return None;
        }
        let hdr = DepacketHeader {
            frame_nr: u64::from_le_bytes(packet_payload[0..8].try_into().ok()?),
            total_len: u32::from_le_bytes(packet_payload[8..12].try_into().ok()?),
            offset: u32::from_le_bytes(packet_payload[12..16].try_into().ok()?),
            chunk_len: u32::from_le_bytes(packet_payload[16..20].try_into().ok()?),
        };

        let payload_end = Self::HEADER_SIZE.checked_add(hdr.chunk_len as usize)?;
        if payload_end > packet_payload.len() {
            return None;
        }

        let data_slice = &packet_payload[Self::HEADER_SIZE..payload_end];

        Some((hdr, data_slice))
    }
}

/// Internal state for reassembling frames
#[derive(Debug)]
pub struct FrameReassembly {
    pub first_chunk_time: Instant,
    pub total_len: u32,
    pub received_len: u32,
    pub buffer: Vec<u8>,
    pub received_mask: BitVec,
}

impl FrameReassembly {
    pub fn new(total_len: u32) -> Self {
        Self {
            first_chunk_time: Instant::now(),
            total_len,
            received_len: 0,
            buffer: vec![0; total_len as usize],
            received_mask: bitvec![0; total_len as usize],
        }
    }

    /// Insert a chunk into the buffer at the given offset.
    /// Return true if the frame is complete.
    pub fn insert_chunk(&mut self, offset: u32, data: &[u8]) -> bool {
        let end = offset as usize + data.len();
        if end <= self.buffer.len() {
            for (i, &byte) in data.iter().enumerate() {
                let idx = offset as usize + i;
                // Only update if this byte was not already received.
                if !self.received_mask[idx] {
                    // Mark bit as received.
                    self.received_mask.set(idx, true);
                    // Write into the buffer.
                    self.buffer[idx] = byte;
                    // Increment counter for newly received bytes.
                    self.received_len += 1;
                }
            }
        }
        // If we have received every byte, we’re done.
        self.received_len >= self.total_len
    }
}

pub struct TrackRemoteSpatialRtp {
    remote_track: Arc<TrackRemote>,
    on_frame: Arc<dyn Fn(FrameTaskData) + Send + Sync>,
    read_task: Option<JoinHandle<()>>,
    parse_task: Option<JoinHandle<()>>,
    cleanup_task: Option<JoinHandle<()>>,
    cancel_token: CancellationToken,
}

impl TrackRemoteSpatialRtp {
    pub fn new(
        remote_track: Arc<TrackRemote>,
        on_frame: Arc<dyn Fn(FrameTaskData) + Send + Sync>,
    ) -> Self {
        Self {
            remote_track,
            on_frame,
            read_task: None,
            parse_task: None,
            cleanup_task: None,
            cancel_token: CancellationToken::new(),
        }
    }

    pub fn start(&mut self) {
        // Spawn a background task that reads from the remote track
        let remote_track = self.remote_track.clone();
        let reassembly_map = Arc::new(DashMap::new());
        let on_frame_cb = self.on_frame.clone();
        let cancel_token = self.cancel_token.clone();
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(200);
        //TODO: close the channel when we stop the track, or some other way to stop the loop

        let read_cancel = cancel_token.clone();
        let read_handle = tokio::spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            loop {
                tokio::select! {
                    _ = read_cancel.cancelled() => {
                        info!("Read task cancelled");
                        break;
                    }
                    else => {
                        // read_rtp blocks until we get a packet or error
                        // TODO: check if that read_rtp yields internally while waiting, normally this await should "yield"
                        let pkt_result = remote_track.read(&mut rtcp_buf).await;
                        let pkt = match pkt_result {
                            Ok(packet) => packet.0,
                            Err(e) => {
                                error!("Error reading RTP from track: {:?}", e);
                                // If it's a permanent error, maybe break
                                break;
                            }
                        };
                        if let Err(e) = tx.send(pkt.payload.into()).await {
                            error!("Error sending RTP packet to parse task: {:?}", e);
                            break;
                        }

                    }
                }
            }
        });

        // TODO: some way to stop the task.
        let parse_cancel = cancel_token.clone();
        let reassembly_map_clone = reassembly_map.clone();
        let parse_handle = tokio::spawn(async move {
            let reassembly_map = reassembly_map_clone;
            while let Some(rtp_packet) = tokio::select! {
                _ = parse_cancel.cancelled() => {
                    info!("Parse task cancelled");
                    None
                }
                packet = rx.recv() => packet,
            } {
                let rtp_packet: Vec<u8> = rtp_packet; // Ensure rtp_packet is owned
                                                      // parse the payload
                if let Some((hdr, chunk)) = DepacketHeader::parse(&rtp_packet) {
                    let key = hdr.frame_nr;
                    let mut can_remove = false;
                    {
                        // Lock the map for writing
                        let mut entry = reassembly_map
                            .entry(key)
                            .or_insert_with(|| FrameReassembly::new(hdr.total_len));
                        let complete = entry.insert_chunk(hdr.offset, chunk);
                        if complete {
                            can_remove = true;
                            // let elapsed_reception_time = entry.first_chunk_time.elapsed();

                            // We have a full frame
                            let full_data = std::mem::take(&mut entry.buffer);

                            // Build a FrameTaskData
                            let ftd = FrameTaskData {
                                presentation_time: hdr.frame_nr, // Normally we store the presentation time in the frame_nr field
                                send_time: hdr.frame_nr, // However, we actually store the send time in the frame_nr field, for metrics purposes
                                payload_metadata: FramePayloadMetadata::default(),
                                data: full_data,
                                client_id: None,
                                quality_index: None,
                            };

                            // info!("Receiving all packets for this frame took: {:?} ms", elapsed_reception_time.as_millis());

                            (on_frame_cb)(ftd);
                        }
                    }
                    if can_remove {
                        // Remove the entry from the map
                        reassembly_map.remove(&key);
                    }
                } else {
                    // parse failed
                    error!(
                        "Failed to parse custom header from RTP packet with length = {}",
                        rtp_packet.len()
                    );
                }
            }
        });

        // A seperate task that cleans up the reassembly_map
        // periodically, removing entries that are too old
        // TODO: some way to stop the task.
        let cleanup_cancel = cancel_token.clone();
        let cleanup_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cleanup_cancel.cancelled() => {
                        info!("Cleanup task cancelled");
                        break;
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                        let now = Instant::now();
                        for entry in reassembly_map.iter() {
                            if now.duration_since(entry.value().first_chunk_time).as_secs() > 60 {
                                reassembly_map.remove(entry.key());
                            }
                        }
                    }
                }
            }
        });

        self.read_task = Some(read_handle);
        self.parse_task = Some(parse_handle);
        self.cleanup_task = Some(cleanup_handle);
    }

    pub async fn stop(&mut self) -> Result<(), RtcError> {
        // Get the track id
        let track_id = self.remote_track.id();
        info!("Stopping TrackRemoteSpatialRtp for track_id: {}", track_id);

        self.cancel_token.cancel(); // signal all loops to break

        // Wait 20 ms, to give tasks time to finish
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        if let Some(h) = self.read_task.take() {
            if !h.is_finished() {
                info!("Stopping read task");
                h.abort();
            }
        } else {
            error!("Read task was already stopped or not initialized");
        }
        if let Some(h) = self.parse_task.take() {
            if !h.is_finished() {
                info!("Stopping parse task");
                h.abort();
            }
        } else {
            error!("Parse task was already stopped or not initialized");
        }
        if let Some(h) = self.cleanup_task.take() {
            if !h.is_finished() {
                info!("Stopping cleanup task");
                h.abort();
            }
        } else {
            error!("Cleanup task was already stopped or not initialized");
        }
        Ok(())
    }
}

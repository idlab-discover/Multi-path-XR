use bitvec::prelude::*;
use webrtc::track::track_remote::TrackRemote;
use webrtc::error::Error as RtcError;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use std::{sync::Arc, time::Instant};
use crate::types::FrameTaskData;
use tracing::error;
use dashmap::DashMap;

/// The same layout as your “PointCloudPayloader” header:
///   [0..4]   client_id
///   [4..8]   frame_nr
///   [8..12]  total_len
///   [12..16] seq_offset
///   [16..20] chunk_len
///   [20..24] tile_nr
///
/// Then chunk_len bytes of data.
#[derive(Clone, Debug, Default)]
pub struct DepacketHeader {
    pub client_id: u32,
    pub frame_nr: u64,
    pub total_len: u32,
    pub offset: u32,
    pub chunk_len: u32,
    pub tile_nr: u32,
    pub quality_nr: u32,
}

impl DepacketHeader {
    pub const HEADER_SIZE: usize = 32;

    pub fn parse(packet_payload: &[u8]) -> Option<(Self, &[u8])> {
        if packet_payload.len() < Self::HEADER_SIZE {
            return None;
        }
        let hdr = DepacketHeader {
            client_id: u32::from_le_bytes(packet_payload[0..4].try_into().ok()?),
            frame_nr: u64::from_le_bytes(packet_payload[4..12].try_into().ok()?),
            total_len: u32::from_le_bytes(packet_payload[12..16].try_into().ok()?),
            offset: u32::from_le_bytes(packet_payload[16..20].try_into().ok()?),
            chunk_len: u32::from_le_bytes(packet_payload[20..24].try_into().ok()?),
            tile_nr: u32::from_le_bytes(packet_payload[24..28].try_into().ok()?),
            quality_nr: u32::from_le_bytes(packet_payload[28..32].try_into().ok()?),
        };

        // The rest is chunk data
        let data_slice = &packet_payload[Self::HEADER_SIZE..Self::HEADER_SIZE + hdr.chunk_len as usize];

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

pub struct TrackRemotePointCloudRTP {
    remote_track: Arc<TrackRemote>,
    on_frame: Arc<dyn Fn(FrameTaskData) + Send + Sync>,
    read_task: Option<JoinHandle<()>>,
    parse_task: Option<JoinHandle<()>>,
    cleanup_task: Option<JoinHandle<()>>,
}

impl TrackRemotePointCloudRTP {
    pub fn new(
        remote_track: Arc<TrackRemote>,
        on_frame: Arc<dyn Fn(FrameTaskData) + Send + Sync>
    ) -> Self {
        Self {
            remote_track,
            on_frame,
            read_task: None,
            parse_task: None,
            cleanup_task: None,
        }
    }

    pub fn start(&mut self) {
        // Spawn a background task that reads from the remote track
        let remote_track = self.remote_track.clone();
        let reassembly_map = Arc::new(DashMap::new());
        let on_frame_cb = self.on_frame.clone();
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(200);

        let read_handle = tokio::spawn(async move {
            let mut rtcp_buf = vec![0u8; 1500];
            loop {
                // read_rtp blocks until we get a packet or error
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
        });


        let reassembly_map_clone = reassembly_map.clone();
        let parse_handle = tokio::spawn(async move {
            let reassembly_map = reassembly_map_clone;
            while let Some(rtp_packet) = rx.recv().await {
                let rtp_packet: Vec<u8> = rtp_packet; // Ensure rtp_packet is owned
                // parse the payload
                if let Some((hdr, chunk)) = DepacketHeader::parse(&rtp_packet) {
                    let key = (hdr.client_id, hdr.frame_nr, hdr.tile_nr, hdr.quality_nr);
                    let mut can_remove = false;
                    {
                        // Lock the map for writing
                        let mut entry = reassembly_map.entry(key).or_insert_with(|| FrameReassembly::new(hdr.total_len));
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
                                data: full_data,
                                sfu_client_id: Some(hdr.client_id as u64),
                                sfu_frame_len: Some(hdr.total_len),
                                sfu_tile_index: Some(hdr.tile_nr),
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
                    error!("Failed to parse custom header from RTP packet with length = {}", rtp_packet.len());
                }
            }
        });

        // A seperate task that cleans up the reassembly_map
        // periodically, removing entries that are too old
        let cleanup_handle = tokio::spawn(async move {
            loop {
                // Sleep for a while before checking the map
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                let now = Instant::now();
                for entry in reassembly_map.iter() {
                    if now.duration_since(entry.value().first_chunk_time).as_secs() > 60 {
                        reassembly_map.remove(entry.key());
                    }
                }
            }
        });


        self.read_task = Some(read_handle);
        self.parse_task = Some(parse_handle);
        self.cleanup_task = Some(cleanup_handle);
    }

    pub async fn stop(&mut self) -> Result<(), RtcError> {
        if let Some(h) = self.read_task.take() {
            if !h.is_finished() {
                h.abort();
            }
        }
        if let Some(h) = self.parse_task.take() {
            if !h.is_finished() {
                h.abort();
            }
        }
        if let Some(h) = self.cleanup_task.take() {
            if !h.is_finished() {
                h.abort();
            }
        }
        Ok(())
    }
}
use std::sync::{Arc, Mutex};

use bytes::{BufMut, Bytes, BytesMut};
use tracing::instrument;
use webrtc::rtp::packetizer::Payloader;

#[derive(Debug)]
struct PointCloudMetadata {
    client_id: u32,
    frame_nr: u64,
    tile_nr: u32,
    quality_nr: u32,
}

#[derive(Debug, Clone)]
pub struct PointCloudPayloader {
    metadata: Arc<Mutex<PointCloudMetadata>>
}

impl PointCloudPayloader {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for PointCloudPayloader {
    fn default() -> Self {
        Self {
            metadata: Arc::new(Mutex::new(PointCloudMetadata {
                client_id: 0,
                frame_nr: 0,
                tile_nr: 0,
                quality_nr: 0,
            })),
        }
    }
}

impl Payloader for PointCloudPayloader {

    #[instrument(skip_all)]
    fn payload(&mut self, mtu: usize, payload_data: &Bytes) -> Result<Vec<Bytes>, webrtc::rtp::Error> {
        if payload_data.is_empty() || mtu <= 28 {
            return Ok(vec![]);
        }

        let payload_len = payload_data.len() as u32;
        const HEADER_SIZE: usize = 28;
        let max_data_per_packet = mtu - HEADER_SIZE;
        let mut output = vec![];
        let mut payload_data_remaining = payload_data.len();
        let mut offset = 0;

        if std::cmp::min(max_data_per_packet, payload_data_remaining) == 0 {
            return Ok(vec![]);
        }
        
        let meta = self.metadata.lock().unwrap();
        let client_id = meta.client_id;
        let frame_nr = meta.frame_nr;
        let tile_nr = meta.tile_nr;
        let quality_nr = meta.quality_nr;
        drop(meta);


        while payload_data_remaining > 0 {
            let chunk_len = std::cmp::min(max_data_per_packet, payload_data.len() - offset);
            let mut out = BytesMut::with_capacity(HEADER_SIZE + chunk_len);

            out.put_u32_le(client_id); // client id
            out.put_u64_le(frame_nr); // Frame counter
            out.put_u32_le(payload_len); // payload len
            out.put_u32_le(offset as u32); // payload data offset of this chunk
            out.put_u32_le(chunk_len as u32); // current chunk size
            out.put_u32_le(tile_nr); // tile
            out.put_u32_le(quality_nr); // quality
            out.put(
                &*payload_data.slice(offset..(offset + chunk_len)),
            );

            output.push(out.freeze());

            offset += chunk_len;
            payload_data_remaining -= chunk_len;
        }

        Ok(output)
    }

    fn clone_to(&self) -> Box<dyn Payloader + Send + Sync> {
        Box::new(PointCloudPayloader {
            metadata: self.metadata.clone(),
        })
    }
}

impl PointCloudPayloader {
    pub fn set_metadata(&mut self, client_id: u32, frame_nr: u64, tile_nr: u32, quality_nr: u32) {
        let mut meta = self.metadata.lock().unwrap();
        meta.client_id = client_id;
        meta.frame_nr  = frame_nr;
        meta.tile_nr   = tile_nr;
        meta.quality_nr = quality_nr;
    }
}

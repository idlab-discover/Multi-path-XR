use std::sync::{Arc, Mutex};

use bytes::{BufMut, Bytes, BytesMut};
use tracing::instrument;
use webrtc::rtp::packetizer::Payloader;

#[derive(Debug)]
struct SpatialRtpMetadata {
    frame_nr: u64,
}

#[derive(Debug, Clone)]
pub struct SpatialRtpPayloader {
    metadata: Arc<Mutex<SpatialRtpMetadata>>,
}

impl SpatialRtpPayloader {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for SpatialRtpPayloader {
    fn default() -> Self {
        Self {
            metadata: Arc::new(Mutex::new(SpatialRtpMetadata { frame_nr: 0 })),
        }
    }
}

impl Payloader for SpatialRtpPayloader {
    #[instrument(skip_all)]
    fn payload(
        &mut self,
        mtu: usize,
        payload_data: &Bytes,
    ) -> Result<Vec<Bytes>, webrtc::rtp::Error> {
        const HEADER_SIZE: usize = 20;
        if payload_data.is_empty() || mtu <= HEADER_SIZE {
            return Ok(vec![]);
        }

        let payload_len = payload_data.len() as u32;
        let max_data_per_packet = mtu - HEADER_SIZE;
        let mut output = vec![];
        let mut payload_data_remaining = payload_data.len();
        let mut offset = 0;

        if std::cmp::min(max_data_per_packet, payload_data_remaining) == 0 {
            return Ok(vec![]);
        }

        let meta = self.metadata.lock().unwrap();
        let frame_nr = meta.frame_nr;
        drop(meta);

        while payload_data_remaining > 0 {
            let chunk_len = std::cmp::min(max_data_per_packet, payload_data.len() - offset);
            let mut out = BytesMut::with_capacity(HEADER_SIZE + chunk_len);

            out.put_u64_le(frame_nr); // Frame counter
            out.put_u32_le(payload_len); // payload len
            out.put_u32_le(offset as u32); // payload data offset of this chunk
            out.put_u32_le(chunk_len as u32); // current chunk size
            out.put(&*payload_data.slice(offset..(offset + chunk_len))); // TODO: how does put compare with put_slice? Which is more efficient?

            output.push(out.freeze());

            offset += chunk_len;
            payload_data_remaining -= chunk_len;
        }

        Ok(output)
    }

    fn clone_to(&self) -> Box<dyn Payloader + Send + Sync> {
        Box::new(SpatialRtpPayloader {
            metadata: self.metadata.clone(),
        })
    }
}

impl SpatialRtpPayloader {
    pub fn set_frame_nr(&mut self, frame_nr: u64) {
        let mut meta = self.metadata.lock().unwrap();
        meta.frame_nr = frame_nr;
    }
}

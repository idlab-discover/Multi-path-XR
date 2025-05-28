use crate::format_fourcc;
use super::{generic::Mp4Box, hdlr::HdlrBox};

/// The `MetaBox` represents metadata information in the MP4 file.
/// This simplified version assumes a default `hdlr` box and ignores extended data.
#[derive(Default, Clone)]
pub struct MetaBox {
    pub hdlr: HdlrBox,  // Handler Box inside Meta
}

impl std::fmt::Debug for MetaBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaBox")
            .field("box_size", &self.box_size())
            .field("box_type", &format_fourcc(&self.box_type()))
            .field("hdlr", &self.hdlr)
            .finish()
    }
}

impl Mp4Box for MetaBox {
    fn box_type(&self) -> [u8; 4] { *b"meta" }

    fn box_size(&self) -> u32 {
        8 + 4 + self.hdlr.box_size()  // header + version/flags + hdlr box
    }

    fn write_box(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&self.box_size().to_be_bytes());
        buffer.extend_from_slice(&self.box_type());
        buffer.extend_from_slice(&0u32.to_be_bytes());  // version + flags = 0
        let current_size = buffer.len();
        let hdlr_size = self.hdlr.box_size() as usize;
        self.hdlr.write_box(buffer);
        if buffer.len() != current_size + hdlr_size {
            panic!("Error writing HdlrBox: expected size {}, got {}", hdlr_size, buffer.len() - current_size);
        }
    }

    fn read_box(data: &[u8]) -> Result<(Self, usize), String> {
        if data.len() < 16 {
            return Err("META box too small".into());
        }

        let size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        if data.len() < size {
            return Err("Incomplete META box".into());
        }

        if &data[4..8] != b"meta" {
            return Err("Not a META box".into());
        }

        let offset = 12;  // Skip header + version/flags

        let (hdlr, _hdlr_size) = HdlrBox::read_box(&data[offset..])?;

        Ok((
            MetaBox { hdlr },
            size
        ))
    }
}
